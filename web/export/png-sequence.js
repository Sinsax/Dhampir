// 逐帧导出 PNG 序列 —— **帧精确**的那条路。
//
// 为什么不用 MediaRecorder 实时录制：那是**实时**的，渲染跟不上就丢帧，
// 而本项目的铁律是帧精确。逐帧渲染一帧一次读回，帧数就是帧数，没有"大约"。
// 编码交给 FFmpeg（下游或本地），这一层只负责把帧**原样**交出去。

function base64ToBytes(base64) {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  // 下标必须是 index。写成 charCodeAt(0) 会把每个字节都变成首字符——
  // 症状是「整张图只有一个字节值」，而长度还是对的，很容易误判成传输问题。
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

/**
 * @param engine  引擎（engine.js）
 * @param project 工程（当前 UI 上这一份，不是 engine 里那份）
 * @param range   { from, to } **闭区间**，单位整数帧
 * @param onFrame (frame, bytes, total) => Promise<void>
 */
export async function exportPngSequence(engine, project, range, onFrame) {
  const canvas = document.getElementById("preview");
  const total = range.to - range.from + 1;
  for (let frame = range.from; frame <= range.to; frame += 1) {
    // 先把这一帧的工程状态推给 engine（UI 可能改过它）。
    engine.open(JSON.stringify(project));
    await engine.seek(frame);
    // 读回 canvas：WebGPU canvas 支持 toDataURL，拿到的是刚刚 present 的那一帧。
    const dataUrl = canvas.toDataURL("image/png");
    const comma = dataUrl.indexOf(",");
    if (comma < 0) throw new Error("第 " + frame + " 帧读回失败：toDataURL 没有数据段");
    const bytes = base64ToBytes(dataUrl.slice(comma + 1));
    await onFrame(frame, bytes, total);
  }
  return total;
}
