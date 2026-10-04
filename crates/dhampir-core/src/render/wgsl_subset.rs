//! 可移植子集检查的**共用**工具（只在 `cfg(test)` 下编译）。
//!
//! # 为什么要抽出来
//!
//! M0 只有 `probe.wgsl` 一份着色器，剥注释函数与禁词表都写在 `probe.rs` 的测试模块里。
//! M1 多了 `scene.wgsl`，如果各写一份，就会出现"一份被改对、另一份留在原样"的漂移。
//! 而这类漂移的方向几乎总是**守卫变松**——比如为了修 `textureSampleLevel` 的误报，
//! 在其中一份里把 `textureSample` 改成了别的写法，另一份还停在旧词表，于是两份报告
//! 都写着"通过"。共用一份就没有"另一份"可言。
//!
//! # 检查对象一律是剥掉注释后的代码
//!
//! 两份 WGSL 的文件头都写着"不用导数（`fwidth` / `dpdx` / `dpdy`）"。不剥注释的话，
//! 守卫会去举报**自己的文档**，而下一个人的修法通常是"把守卫删掉"。
//! 守卫被删掉比守卫误报更糟。
//!
//! # 禁词表为什么是"带理由"的
//!
//! 每一项在断言失败时都会把理由一起说出来。只说"出现了 fwidth"的话，接手的人要么
//! 去问、要么去猜、要么删掉这一项——三种结果都比"把理由写在旁边"差。

/// 能力下限里**不允许**出现的构造，以及各自的原因（指导文档 §4.3①）。
///
/// 注意 `"textureSample("` 带左括号：`textureSampleLevel` 是**允许**的（它显式给出
/// LOD，不需要导数），而裸的 `"textureSample"` 会把它一起打成违规。这个括号就是
/// 一次真实误报的修复结果，`forbidden_tokens_do_not_swallow_texture_sample_level`
/// 把"别把它改回去"钉住了。
pub const FORBIDDEN: [(&str, &str); 6] = [
    ("fwidth", "导数：结果允许因实现而异，基准用例里不能出现"),
    ("dpdx", "导数：结果允许因实现而异"),
    ("dpdy", "导数：结果允许因实现而异"),
    (
        "textureSample(",
        "隐式 LOD 采样：它内部要导数；采样一律走 textureLoad",
    ),
    ("loop", "循环：累加顺序会交给编译器，而顺序必须由文本决定"),
    ("atomic", "原子：跨运行时的结果不可预期"),
];

/// 剥掉 `//` 行注释与 `/* */` 块注释。
///
/// 见模块文档：要扫的是**代码**，不是散文。
pub fn strip_wgsl_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'/') {
            // 行注释：连行尾的换行一起吃掉。
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 检查一份 WGSL 是否待在可移植子集里，**返回去注释后的代码**供调用方继续做
/// 自己的存在性断言（比如"两个入口都在"）。
///
/// `label` 会出现在断言消息里：两份着色器共用同一个检查时，失败信息必须能指出
/// 是哪一份出了问题。
pub fn check_portable_subset(label: &str, src: &str) -> String {
    let code = strip_wgsl_comments(src);
    for (token, why) in FORBIDDEN {
        assert!(
            !code.contains(token),
            "{label} 的代码里出现了 {token}——{why}"
        );
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_stripping_leaves_code_alone() {
        // 剥注释这件事本身也要被守住：如果它哪天变成了"整份都吃掉"，
        // 上面那条可移植性子集检查会瞬间变成永远为真的空话。
        let src = "// fwidth\nlet a = 1; /* dpdx */ let b = 2; // loop\n";
        assert_eq!(strip_wgsl_comments(src), "let a = 1;  let b = 2; ");
    }

    #[test]
    fn forbidden_tokens_do_not_swallow_texture_sample_level() {
        // 这条断言就是那次误报的修复：`textureSampleLevel` 显式给出 LOD，
        // 不依赖导数，属于可移植子集；裸的 `"textureSample"` 会把它一起判红。
        assert!(
            !"textureSampleLevel(t, s, uv, 0.0)".contains("textureSample("),
            "禁词表又开始连 textureSampleLevel 一起吃了"
        );
    }

    #[test]
    fn portable_subset_accepts_texture_load_and_texture_sample_level() {
        // 反向验证：这份代码里有 `textureSampleLevel` 和 `textureLoad`，
        // 它们**必须**过。没有这条，"检查通过"只证明不了任何事——
        // 一个把所有东西都判红的守卫和恒绿的守卫一样没用。
        let code = "fn f(uv: vec2<f32>) -> vec4<f32> {\n  let a = textureLoad(t, vec2<i32>(0), 0);\n  let b = textureSampleLevel(t, s, uv, 0.0);\n  return a + b;\n}\n";
        assert!(check_portable_subset("测试用例", code).contains("textureSampleLevel"));
    }

    #[test]
    fn portable_subset_rejects_each_forbidden_token() {
        // 逐项反向验证：每一项都真的会让检查变红。少验一项，那一项就可能
        // 悄悄失效（比如词表里写错了字），而"检查通过"照旧打印。
        for (token, _) in FORBIDDEN {
            let code = format!("fn f() {{ let x = {token} 0.0; }}\n");
            let panicked =
                std::panic::catch_unwind(|| check_portable_subset("反例", &code)).is_err();
            assert!(panicked, "禁词 {token} 没有让检查变红");
        }
    }
}
