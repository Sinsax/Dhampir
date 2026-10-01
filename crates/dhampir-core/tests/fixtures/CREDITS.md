# 测试夹具出处

## animated_lossless.webp

* 来源：<https://github.com/image-rs/image-webp> 的 `tests/images/animated/random_lossless.webp`
* 许可：MIT OR Apache-2.0（与 `image-webp` 这个 crate 同许可）
* 为什么用真文件而不是自造的字节：动画 WebP 没有纯 Rust 编码器，
  手写 VP8L 位流才能自造 —— 那不是测试该干的事。
  这份夹具同时被 `image-webp` 自己的 reftest 用着，所以它"是对的"这件事有出处。
* 用例：`dhampir-core/src/animation.rs` 里 `webp_动画整段解码且与文件自报的账对得上`
  与 `静态字节流进不来_动图这条路只收动图`。
