# 全部枚举效果：索引

工程：out/effects-demo/effects-demo.doc.json（三份产物共用这一份）
长度：860 帧 @ 30 fps（28.7 秒），文档尺寸 640x360

| 区段 | 条目 | 图层 | 帧区间 | 缓动 |
|---|---|---|---|---|
| A | 混合 normal | blend-top-normal | 0-20 | - |
| A | 混合 add | blend-top-add | 20-40 | - |
| A | 混合 multiply | blend-top-multiply | 40-60 | - |
| A | 混合 screen | blend-top-screen | 60-80 | - |
| A | 混合 darken | blend-top-darken | 80-100 | - |
| A | 混合 lighten | blend-top-lighten | 100-120 | - |
| A | 混合 overlay | blend-top-overlay | 120-140 | - |
| A | 混合 soft_light | blend-top-soft_light | 140-160 | - |
| A | 混合 difference | blend-top-difference | 160-180 | - |
| A | 裁剪：圆 | feat-clip-circle | 180-200 | - |
| A | 裁剪：椭圆 | feat-clip-ellipse | 200-220 | - |
| A | 裁剪：内缩矩形（带圆角） | feat-clip-inset | 220-240 | - |
| A | 裁剪：多边形 | feat-clip-polygon | 240-260 | - |
| A | 裁剪：路径 | feat-clip-path | 260-280 | - |
| A | 圆角 60px | feat-corner-60 | 280-300 | - |
| A | 掩码：程序化线性渐变 | feat-mask-gradient | 300-320 | - |
| A | 掩码：素材（alpha 通道） | feat-mask-asset | 320-340 | - |
| A | 投影（向外扩散） | feat-shadow | 340-360 | - |
| A | 背景滤镜（读身后内容再糊） | feat-backdrop | 360-380 | - |
| A | 缓动 linear | ease-0 | 380-400 | linear |
| A | 缓动 cubic-bezier(0.25,0.1,0.25,1) | ease-1 | 400-420 | cubic-bezier(0.25,0.1,0.25,1) |
| A | 缓动 ease-in-out | ease-2 | 420-440 | ease-in-out |
| A | 缓动 back_out | ease-3 | 440-460 | back_out |
| A | 缓动 steps(4) | ease-4 | 460-480 | steps(4) |
| A | 缓动 linear(0, 0.25 75%, 1) | ease-5 | 480-500 | linear(0, 0.25 75%, 1) |
| A | 缓动 cubic-bezier(0.68,-0.55,0.265,1.55) | ease-6 | 500-520 | cubic-bezier(0.68,-0.55,0.265,1.55) |
| A | 缓动 ease-in | ease-7 | 520-540 | ease-in |
| B | 效果 gaussian_blur（模糊（可分离两趟）） | fx-gaussian_blur | 540-560 | linear |
| B | 效果 brightness（亮度（加性）） | fx-brightness | 560-580 | cubic-bezier(0.25,0.1,0.25,1) |
| B | 效果 brightness_multiply（亮度（乘性、保黑；对应 CSS brightness）） | fx-brightness_multiply | 580-600 | ease-in-out |
| B | 效果 contrast（对比度（绕 0.5 缩放）） | fx-contrast | 600-620 | back_out |
| B | 效果 saturation（饱和度（Rec.709 权重）） | fx-saturation | 620-640 | steps(4) |
| B | 效果 saturation_css（饱和度（CSS/SVG 规范权重）） | fx-saturation_css | 640-660 | linear(0, 0.25 75%, 1) |
| B | 效果 hue（色相（YIQ / Rec.601 矩阵）） | fx-hue | 660-680 | cubic-bezier(0.68,-0.55,0.265,1.55) |
| B | 效果 hue_rotate_css（色相（CSS/SVG 规范矩阵）） | fx-hue_rotate_css | 680-700 | ease-in |
| B | 效果 flash（闪白/闪色（常量色叠加）） | fx-flash | 700-720 | linear |
| B | 效果 vignette（暗角） | fx-vignette | 720-740 | cubic-bezier(0.25,0.1,0.25,1) |
| B | 效果 noise（噪声） | fx-noise | 740-760 | ease-in-out |
| B | 效果 overlay（渐变叠加） | fx-overlay | 760-780 | back_out |
| B | 效果 shake（抖动（Warp）） | fx-shake | 780-800 | steps(4) |
| B | 效果 zoom_bounce（缩放弹跳（Warp）） | fx-zoom_bounce | 800-820 | linear(0, 0.25 75%, 1) |
| B | 效果 pulse（脉动（Warp）） | fx-pulse | 820-840 | cubic-bezier(0.68,-0.55,0.265,1.55) |
| B | 效果 split（错切分裂（Warp）） | fx-split | 840-860 | ease-in |

## 计数

- 效果（登记表 16 条）：16
- 混合模式（9 条）：9
- 裁剪形状（5 种）：5
- 轨道数：3，图层总数：102
