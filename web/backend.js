// 前端与"后端"之间的那条缝。
//
// # 为什么要有这一层
//
// 三种部署形态（分离 / 本机 / 降级）**共用同一套前端**，差别只在"后端在哪"。
// 所以前端不许自己知道工程从哪来、素材从哪来 —— 那些一律问这里。
//
// 判定标准很硬：**页面里搜不到硬编码的素材路径**。
// 现在是参数与工厂，将来加一种后端只加一个工厂函数，不动页面逻辑。
//
// # 为什么默认是 static
//
// `static` 是**降级模式**：工程与素材都是同一源下的固定 URL，不依赖任何服务。
// 它保证"没有后端也能用"。本机模式与分离模式都是它的替换品，不是它的前置。

/** 降级模式：工程与素材都在同一源下。 */
export function createStaticBackend(options) {
  const settings = options || {};
  const projectUrl = settings.projectUrl || "/sample-project.json";
  const mediaUrl = settings.mediaUrl || "/media/proxy.mp4";
  return {
    kind: "static",
    projectUrl: projectUrl,
    mediaUrl: mediaUrl,
    async loadProject() {
      const response = await fetch(projectUrl);
      if (!response.ok) throw new Error("取工程失败：" + response.status + " " + projectUrl);
      return response.text();
    },
    // 降级模式只有一份素材，所有 source 都用它。
    // **这是它的局限，不是通用做法** —— 真正的多素材要 backend 按 asset 解析。
    async mediaUrlFor() {
      return mediaUrl;
    },
    // 降级模式没有能力声明可说：它不渲染、不出片。
    async capabilities() {
      return null;
    },
  };
}

/** 本机/远端模式：工程与素材都问后端。 */
export function createLocalBackend(baseUrl, projectId) {
  const defaultProject = projectId || 'sample-project';
  const base = String(baseUrl || "").replace(/\/$/, "");
  return {
    kind: "local",
    baseUrl: base,
    projectId: defaultProject,
    async loadProject(projectId) {
      // 没给就用手上这一个 —— 否则会拼出 /projects/undefined，而那是 404 不是报错。
      const wanted = projectId || defaultProject;
      const response = await fetch(base + "/projects/" + encodeURIComponent(wanted));
      if (!response.ok) throw new Error("取工程失败：" + response.status);
      return response.text();
    },
    async mediaUrlFor(assetId) {
      // 素材地址由后端给；前端**不拼**本地路径 —— 那是宿主的事。
      return base + "/assets/" + encodeURIComponent(assetId) + "/media";
    },
    async capabilities() {
      const response = await fetch(base + "/capabilities");
      if (!response.ok) throw new Error("取能力声明失败：" + response.status);
      return response.json();
    },
  };
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
  // 默认降级模式：不依赖任何服务也能用。
  return createStaticBackend();
}
