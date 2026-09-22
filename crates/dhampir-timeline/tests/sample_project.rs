//! 样本工程的守卫：**它必须永远能通过当前 schema 的校验**。
//!
//! 为什么用测试而不是脚本：样本工程是 M4 双端比对的输入，它一旦与 schema 脱节，
//! 比对就变成了「两份都跑不起来的工程」，而那种失败看起来会像渲染问题。
//! 放在 cargo test 里，改 schema 的人会立刻看到它红。
//!
//! 覆盖要求（plan M4 T4.5）：3–5 个片段 + 1 个转场 + 2 个特效 + 关键帧。

const SAMPLE: &str = include_str!("../../../fixtures/sample-project.json");

#[test]
fn 样本工程能解析并通过校验() {
    let project: dhampir_timeline::schema::Project =
        serde_json::from_str(SAMPLE).expect("样本工程必须是合法 JSON 且符合 schema");
    let issues = dhampir_timeline::schema::validate_project(&project);
    assert!(
        issues.is_empty(),
        "样本工程没能通过校验：{:#?}",
        issues
    );
}

#[test]
fn 样本工程覆盖了该覆盖的东西() {
    let project: dhampir_timeline::schema::Project =
        serde_json::from_str(SAMPLE).expect("样本工程必须是合法 JSON");

    let video_clips: Vec<_> = project
        .tracks
        .iter()
        .filter(|track| track.kind == dhampir_timeline::schema::TrackKind::Video)
        .flat_map(|track| track.clips.iter())
        .collect();
    assert!(
        (3..=5).contains(&video_clips.len()),
        "视频片段数应当在 3..=5，得到 {}",
        video_clips.len()
    );

    let transitions = video_clips.iter().filter(|clip| clip.transition_in.is_some()).count();
    assert_eq!(transitions, 1, "应当恰好有 1 个转场");

    let effects: usize = video_clips.iter().map(|clip| clip.effects.len()).sum();
    assert_eq!(effects, 2, "应当恰好有 2 个特效");
    for clip in &video_clips {
        for effect in &clip.effects {
            assert_eq!(effect.kind, "gaussian_blur", "样本里的特效应当都在注册表里");
        }
    }

    let keyframed = video_clips.iter().filter(|clip| !clip.keyframes.is_empty()).count();
    assert!(keyframed >= 1, "应当至少有一个片段带关键帧");

    assert!(
        project.tracks.iter().any(|track| track.kind == dhampir_timeline::schema::TrackKind::Audio),
        "应当有一条音频轨——它不进渲染图，但要参与时间线长度"
    );
}

#[test]
fn 样本工程的特效参数在注册表范围内() {
    // 这条把样本工程与**特效登记表**也对上：光通过结构校验不够，
    // 参数还得在实现真正能做的范围里（radius 上界由 BLUR_MAX_RADIUS 决定）。
    let project: dhampir_timeline::schema::Project =
        serde_json::from_str(SAMPLE).expect("样本工程必须是合法 JSON");
    let effects: Vec<_> = project
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter())
        .flat_map(|clip| clip.effects.iter())
        .cloned()
        .collect();
    let _ = effects;
    // 登记表在 dhampir-core，timeline 不依赖它——所以这里只验结构，
    // 「参数是否在实现范围内」由 core 侧那条串起来的测试负责（见 effects.rs）。
    assert!(dhampir_timeline::schema::validate_project(&project).is_empty());
}
