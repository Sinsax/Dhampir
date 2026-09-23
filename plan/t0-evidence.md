# T0 证据：预览侧判定回传通道

写于 T0 收口时。这份文件是 [defects.md](./defects.md) 里 D8 与 A8 的**证据文件** ——
台账里标 done 的条目必须给出一份**存在的**证据，而且证据要是**原样跑出来的**，不是转述。

## 1. 判据（此前从未成立的那一条）

    同一次编辑，预览（wasm 宿主）与 CLI（native 宿主）必须给出同一个工程。

**为什么它此前不成立**：唯一的观测入口是 CDP 的 Runtime.evaluate，
而 awaitPromise 与 returnByValue 同用时在本机 Chrome 上给回空对象
（scripts/web-check.mjs 里为这件事留了注释）。一个"返回空对象"的诊断工具，
看起来和"什么都没发生"一模一样 —— 所以预览侧的编辑路径一直只能靠肉眼看。

**现在的通道**：页面把判定 POST 到本机后端的 /verdict，驱动只读后端。
**拿不到就是没拿到，不算通过。**

## 2. 正面：跑通

    $ node scripts/web-check.mjs --local --verdict trim-parity
    → http://127.0.0.1:55780/?backend=local&port=8802&project=sample-project.doc&verdict=trim-parity
      后端：http://127.0.0.1:8802（页面用 backend=local）
      浏览器：C:/Program Files/Google/Chrome/Application/chrome.exe
      调试端口：55783
    判定回传：trim-parity
      op：{"op":"trim","layer":"a","edge":"out","to":15}
      ✓ 预览与 CLI 对同一个 op 给出逐字段相同的工程
    EXIT=0

页面自己挑的 op 是「把第一条视频轨第一个元素的出点收到中间」（layer a，end 收到 15）。
op 由页面回传，所以 driver 不会另抄一份「怎么挑元素」，两边不可能挑到不同的元素。

## 3. 反面：CLI 不能用时必须红（反向验证）

    $ node scripts/web-check.mjs --local --verdict trim-parity --cli "C:/Program Files/nodejs/node.exe"
    判定回传：trim-parity
      op：{"op":"trim","layer":"a","edge":"out","to":15}
      - CLI 编辑退出 1：Error: Cannot find module 'F:\para\Code\Dhampir\edit'
    EXIT=1

这条同时证明了两件事：① 判据**能变红**；② 变红时**点名理由**，而不是只说"失败了"。

还有一次真实的反例（不是构造的）：第一版把「页面那份工程」与 fixture 原始文件做了全等比较，
结果正确地报了红 ——

    - 页面与 fixture 的起点就不一样：$.timeline.tracks[0].layers[0]：
      键不同 [end,id,note,source,start] != [blend,effects,enabled,end,id,...]

根因是**工程文件省略了取默认值的字段**，而宿主里那份是引擎重新序列化出来的、把它们补齐了。
所以起点对照改成「文件 ⊑ 宿主」（firstSubsetDifference）：文件里写了的字段必须一致，
宿主多出来的默认字段不算差异。这不是放宽判据 —— 判据的**结论**那一步（after 对 after）
仍然是**全等**比较。

## 4. 自检

    $ node scripts/verdict-compare.mjs --self-test
    OK 判定比较器自检通过（14 条断言）
    EXIT=0

    $ node scripts/dhampir-local.mjs --self-test
    OK 本机后端自检通过（32 条断言）        （T0 之前是 26 条）
    EXIT=0

    $ node scripts/check-defects.mjs
    OK 缺陷台账自洽（条目 26 条，路线图引用齐全，证据路径都存在）
    EXIT=0

    $ node scripts/check-defects.mjs --self-test
    OK 台账守卫自检通过（19 条断言）
    EXIT=0

比较器的自检里有反向用例（值不同、缺键、数组长度不同、数字与字符串、宿主缺键、子集长度不符），
所以它不是一个恒真的检查。台账守卫的自检里同样有 17 条**必须变红**的反向用例。

## 5. T0 改了什么（一览）

| 文件 | 改动 |
|---|---|
| plan/defects.md | 新建：缺陷与架构缺失台账（D1–D16、A1–A10） |
| plan/roadmap.md | 新建：阶段计划 T0–T7，与台账互相引用 |
| scripts/check-defects.mjs | 新建：第 9 个守卫（--self-test、反向用例、拒绝空集、只设 exitCode） |
| scripts/verdict-compare.mjs | 新建：判定比较器（纯函数 + --self-test） |
| scripts/dhampir-local.mjs | 新路由 POST /verdict 与 GET /verdict；自检加 6 条断言 |
| web/app.js | window.dhampir.reportVerdict / runTrimParity；?verdict=<name> 钩子 |
| scripts/web-check.mjs | --verdict <name>（独立流程）、--cli 严格化、findCli/runCliParity/reportVerdict |
| plan/remaining-work.md | M2 时代内容加指针（不删历史） |
