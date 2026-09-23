// A 模式（前后端分离）的导出接口。
//
// **本仓库只提供接口 + 一个 echo 假后端**——真正的服务端不在这个仓库里。
// 契约写在下面，下游照着实现即可；假后端用来证明这条链路是通的。
//
// # 契约
//
//   POST {base}/export
//     request : { project: <工程文件>, from: int, to: int,
//                 format: "mp4"|"webm", width: int, height: int }
//     response: { job_id: string }       —— 异步：出片是长任务，不让 HTTP 连接扛着
//
//   GET  {base}/export/{job_id}
//     response: { job_id: string, state: "queued"|"running"|"succeeded"|"failed"|"cancelled",
//                 progress?: 0..1, download_url?: string, error?: <Issue> }
//
//   取消：DELETE {base}/export/{job_id}
//
// **键名与 Rust 侧的 ExportStatusView 逐字一致**（snake_case，download_url）。
// 之前这里写的是 jobId / downloadUrl —— 那是照着"看起来该这样"编的，
// 与契约对不上，而表现是"提交成功之后永远查不到结果"。
//
// 为什么是"提交 + 轮询"而不是一个长连接把成片吐回来：
// 出片可能几分钟，长连接要处理超时、重连、断点，而任务查询本来就要有（用户会关页面）。

export function createHttpBackend(baseUrl) {
  const base = baseUrl.replace(/\/$/, "");
  return {
    async submit(project, range, options) {
      const response = await fetch(base + "/export", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          project,
          from: range.from,
          to: range.to,
          format: (options && options.format) || "mp4",
          width: (options && options.width) || 1920,
          height: (options && options.height) || 1080,
        }),
      });
      if (!response.ok) throw new Error("提交失败：HTTP " + response.status);
      return response.json();
    },

    async poll(jobId) {
      const response = await fetch(base + "/export/" + encodeURIComponent(jobId));
      if (!response.ok) throw new Error("查询失败：HTTP " + response.status);
      return response.json();
    },

    async cancel(jobId) {
      await fetch(base + "/export/" + encodeURIComponent(jobId), { method: "DELETE" });
    },

    /** 提交 + 轮询到结束。onProgress(比例)。options 透传给 submit（format/width/height）。 */
    async exportProject(project, range, onProgress, options) {
      const submitted = await this.submit(project, range, options);
      const jobId = submitted.job_id;
      if (jobId === undefined) throw new Error("服务端没返回 job_id");
      for (;;) {
        await new Promise((resolve) => setTimeout(resolve, 500));
        const status = await this.poll(jobId);
        if (typeof onProgress === "function") {
          // progress 缺省表示**未知**，不是 0 —— 传 null 让界面显示不确定态。
          onProgress(typeof status.progress === "number" ? status.progress : null, status.state);
        }
        if (status.state === "succeeded") return status;
        if (status.state === "failed" || status.state === "cancelled") {
          const reason = status.error && status.error.message ? status.error.message : status.state;
          throw new Error(reason);
        }
      }
    },

    /** 把后端给的相对下载地址变成可点的绝对地址。 */
    downloadUrl(status) {
      const raw = status && status.download_url;
      if (typeof raw !== "string" || raw.length === 0) return null;
      if (/^https?:\/\//.test(raw)) return raw;
      return base + (raw.startsWith("/") ? raw : "/" + raw);
    },
  };
}

/**
 * echo 假后端：**只用于验收这条链路**，不出片。
 *
 * 它把请求原样记下来并立刻报 done，于是"提交 → 轮询 → 拿到结果"这条路径
 * 在服务端还没写的时候就能被验。
 */
export function createEchoBackend() {
  const jobs = new Map();
  let counter = 0;
  return {
    async submit(project, range, options) {
      counter += 1;
      const jobId = "echo-" + counter;
      jobs.set(jobId, {
        state: "succeeded",
        progress: 1,
        download_url: null,
        echo: {
          clips: project.tracks.reduce((sum, track) => sum + track.clips.length, 0),
          frames: range.to - range.from + 1,
          format: (options && options.format) || "mp4",
        },
      });
      return { jobId };
    },
    async poll(jobId) {
      const job = jobs.get(jobId);
      if (job === undefined) throw new Error("没有这个任务：" + jobId);
      return job;
    },
    async cancel(jobId) { jobs.delete(jobId); },
    async exportProject(project, range, onProgress) {
      const submitted = await this.submit(project, range);
      if (typeof onProgress === "function") onProgress(1, "succeeded");
      return this.poll(submitted.job_id);
    },
  };
}
