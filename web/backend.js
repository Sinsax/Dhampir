// 前端与"后端"之间的那条缝。
//
// # 为什么要有这一层
//
// 三种部署形态**共用同一套前端**，差别只在"后端在哪"：
//
//   static  降级：工程与素材都是同一源下的固定 URL，不依赖任何服务
//   local   本机：后端跑在 localhost
//   remote  分离：后端在别处，URL 由宿主给
//
// 所以前端不许自己知道工程从哪来、素材从哪来 —— 那些一律问这里。
//
// 判定标准很硬：**页面里搜不到硬编码的素材路径**。
//
// # local 与 remote 是同一个东西
//
// 它们是**同一个工厂**，只差 URL 从哪来。这不是省事：分离模式与本机模式的差别
// 本来就只是"后端在哪"，如果它们的客户端实现不同，那就是两套要各自维护的客户端。
//
// # 为什么默认是 static
//
// static 保证"没有后端也能用"。本机模式与分离模式都是它的**替换品**，不是前置。

/** 降级模式：工程与素材都在同一源下。 */
export function createStaticBackend(options) {
  const settings = options || {};
  const projectUrl = settings.projectUrl || "/sample-project.doc.json";
  const mediaUrl = settings.mediaUrl || "/media/proxy.mp4";
  return {
    kind: "static",
    projectUrl: projectUrl,
    mediaUrl: mediaUrl,
    // 降级模式没有 baseUrl：它不渲染也不出片，导出只能走 PNG 序列。
    baseUrl: null,
    async loadProject() {
      const response = await fetch(projectUrl);
      if (!response.ok) throw new Error("取工程失败：" + response.status + " " + projectUrl);
      return response.text();
    },
    // 降级模式只有一份素材，**所有 asset id 都用它**。
    // 这是它的局限，不是通用做法 —— 真正的多素材要后端按 asset 解析。
    async mediaUrlFor() {
      return mediaUrl;
    },
    // 降级模式没有能力声明可说：它不渲染、不出片。
    async capabilities() {
      return null;
    },
  };
}

/** 有后端的模式：工程与素材都问后端。 */
export function createBackendAt(kind, baseUrl, projectId) {
  const defaultProject = projectId || "sample-project";
  const base = String(baseUrl || "").replace(/\/$/, "");
  return {
    kind: kind,
    baseUrl: base,
    projectId: defaultProject,
    async loadProject(wanted) {
      // 没给就用手上这一个 —— 否则会拼出 /projects/undefined，而那是 404 不是报错。
      const id = wanted || defaultProject;
      const response = await fetch(base + "/projects/" + encodeURIComponent(id));
      if (!response.ok) throw new Error("取工程失败：" + response.status);
      return response.text();
    },
    // 素材地址由后端按 **asset id** 给；前端**不拼**本地路径 —— 那是宿主的事。
    async mediaUrlFor(assetId) {
      return base + "/assets/" + encodeURIComponent(assetId) + "/media";
    },
    async capabilities() {
      const response = await fetch(base + "/capabilities");
      if (!response.ok) throw new Error("取能力声明失败：" + response.status);
      return response.json();
    },
  };
}

/** 本机模式：后端在 localhost。 */
export function createLocalBackend(baseUrl, projectId) {
  return createBackendAt("local", baseUrl, projectId);
}

/** 分离模式：后端在别处，URL 由宿主通过查询串给。 */
export function createRemoteBackend(baseUrl, projectId) {
  return createBackendAt("remote", baseUrl, projectId);
}

/** 从查询串决定用哪个后端。**这是唯一的"后端在哪"的判定点。** */
export function activeBackendFrom(search) {
  const params = new URLSearchParams(search || "");
  const kind = params.get("backend");
  if (kind === "local") {
    const projectId = params.get("project") || undefined;
    const port = params.get("port") || "8791";
    return createLocalBackend("http://127.0.0.1:" + port, projectId);
  }
  if (kind === "remote") {
    const url = params.get("url");
    // **没给 url 的 remote 退回降级**，与"认不出的 backend 名退回降级"同一条规矩：
    // 猜一个地址比没有地址更危险 —— 那会连到别的东西上而没人知道。
    if (url) return createRemoteBackend(url, params.get("project") || undefined);
    return createStaticBackend();
  }
  // 默认降级模式：不依赖任何服务也能用。
  return createStaticBackend();
}
