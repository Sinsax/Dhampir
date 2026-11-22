//! CBDT / CBLC（彩色位图 emoji）读取 —— 把字形**自带的 PNG strike** 抽出来。
//!
//! # 为什么不用 libass
//!
//! 出片走 libass（`subtitles=`，见 `text_raster` 模块头），但 libass 对 CBDT 字体
//! **加载都过不去**（实测 0.17.5：`Error getting metadata for embedded font` ⇒ fontselect
//! 回落到系统 CJK 字体 ⇒ emoji 豆腐）。而且就算能加载，出片链路还要把白墨
//! **染色**成样式色（`tint`）—— 彩色位图会被染成单色。所以彩色 emoji 走**独立通道**：
//! 直接从字体文件里抽出 glyph 的 PNG strike（Noto Color Emoji 的 CBDT 条目**本身就是 PNG**），
//! 解码、缩放、按直排 alpha 合成 —— **与浏览器（前端预览）用的是同一份位图数据**。
//!
//! # 只实现实际需要的那部分
//!
//! Noto Color Emoji（含用户库里的 `NotoColorEmoji-CBDT`，2020-09-16 版）实测只有：
//! cmap format 12、CBLC 一个 strike（136ppem）、三个 index subtable（format 1 + imageFormat 17）。
//! 这里只认这套组合，遇到别的布局返回 `None`（调用方回落轮廓/豆腐，**不许**报错）。
//! 字段语义的踩坑记录见模块内各常量的注释（CBLC 的偏移基准是本模块最大的坑）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// 解析结果里我们关心的三张表的位置与长度。
#[derive(Debug, Clone, Copy)]
struct Tables {
    cmap: (usize, usize),
    cblc: (usize, usize),
    cbdt: (usize, usize),
}

fn u16_at(d: &[u8], off: usize) -> Option<u16> {
    d.get(off..off + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], off: usize) -> Option<u32> {
    d.get(off..off + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// 这支字体是不是「彩色位图 emoji」（有 CBDT + CBLC 表）。
///
/// 判据很便宜（只看表目录），可以在挑字体的时候用 —— 用户装了彩色 emoji 字体时，
/// emoji 码位优先走它（与浏览器 `Emoji_Presentation` 的行为一致）。
pub fn has_color_bitmap(path: &Path) -> bool {
    with_tables(path, |_| true).unwrap_or(false)
}

type TablesMap = std::sync::Mutex<HashMap<PathBuf, Option<Tables>>>;

/// 缓存：同一个文件只解析一次表目录。
fn tables_cache() -> &'static TablesMap {
    static C: OnceLock<TablesMap> = OnceLock::new();
    C.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn with_tables<T>(path: &Path, f: impl FnOnce(&Tables) -> T) -> Option<T> {
    let mut guard = tables_cache().lock().unwrap_or_else(|e| e.into_inner());
    if !guard.contains_key(path) {
        let t = scan_tables(path);
        guard.insert(path.to_path_buf(), t);
    }
    let t = guard.get(path).cloned()??;
    drop(guard);
    Some(f(&t))
}

/// 读表目录，确认 `cmap` / `CBLC` / `CBDT` 三张表都在（缺一 = 不是彩色位图字体）。
fn scan_tables(path: &Path) -> Option<Tables> {
    let data = std::fs::read(path).ok()?;
    let num = u16_at(&data, 4)? as usize;
    let mut t = Tables {
        cmap: (0, 0),
        cblc: (0, 0),
        cbdt: (0, 0),
    };
    let mut found = 0u8;
    for i in 0..num {
        let rec = 12 + 16 * i;
        let tag = data.get(rec..rec + 4)?;
        let off = u32_at(&data, rec + 8)? as usize;
        let len = u32_at(&data, rec + 12)? as usize;
        match tag {
            b"cmap" => {
                t.cmap = (off, len);
                found += 1;
            }
            b"CBLC" => {
                t.cblc = (off, len);
                found += 1;
            }
            b"CBDT" => {
                t.cbdt = (off, len);
                found += 1;
            }
            _ => {}
        }
    }
    (found == 3).then_some(t)
}

/// 一张解码好的彩色位图（**直排 alpha**，未预乘 —— 调用方负责预乘）。
pub struct ColorStrike {
    pub width: u32,
    pub height: u32,
    /// RGBA，每像素 4 字节。
    pub pixels: Vec<u8>,
    /// 位图相对**基线**的摆放（像素，上为正）与水平步进。
    ///
    /// `bearing_y` 是「位图顶边在基线上方多少像素」，`advance_px` 是这个字形
    /// 在字体设计尺寸（`ppem`）下占的水平宽度。
    pub bearing_x: i32,
    pub bearing_y: i32,
    /// strike 自身像素尺寸下的水平步进（Noto：= strike 宽 ⇒ 1em）。
    pub advance_px: u32,
}

/// 拿一个码点的彩色位图。
///
/// 全流程：cmap(format 12) → glyph id → CBLC 定 strike 与 index subtable →
/// CBDT 里读 `SmallGlyphMetrics(5B) + dataLen(4B) + PNG`。任何一步不认识
/// （别的 format / 别的字体的怪布局）都返回 `None`。
pub fn color_strike(path: &Path, codepoint: u32) -> Option<ColorStrike> {
    let data = std::fs::read(path).ok()?;
    let t = with_tables(path, |t| *t)?;
    let gid = glyph_id_cmap12(&data, t.cmap, codepoint)?;
    strike_for_glyph(&data, t, gid)
}

/// cmap format 12 的 `(码点 → glyph id)`。
fn glyph_id_cmap12(data: &[u8], (off, _len): (usize, usize), cp: u32) -> Option<u32> {
    let n = u16_at(data, off + 2)? as usize;
    // 找 (3,10) 子表；找不到就找第一个 format 12 的。
    let mut best: Option<usize> = None;
    for i in 0..n {
        let rec = off + 4 + 8 * i;
        let pid = u16_at(data, rec)?;
        let eid = u16_at(data, rec + 2)?;
        let sub = u32_at(data, rec + 4)? as usize;
        let fmt = u16_at(data, off + sub)?;
        if fmt == 12 && ((pid, eid) == (3, 10) || best.is_none()) {
            best = Some(off + sub);
            if (pid, eid) == (3, 10) {
                break;
            }
        }
    }
    let sub = best?;
    let groups = u32_at(data, sub + 12)? as usize;
    // 组数通常 ~3500，线性扫够用（单次调用 + 有 glyph 缓存的调用方）；
    // 要更快可以二分 —— groups 按 startCharCode 升序。
    for g in 0..groups {
        let b = sub + 16 + 12 * g;
        let start = u32_at(data, b)?;
        let end = u32_at(data, b + 4)?;
        if start > cp {
            break; // 升序：后面只会更大
        }
        if end >= cp {
            let sg = u32_at(data, b + 8)?;
            return Some(sg + (cp - start));
        }
    }
    None
}

/// 在 CBLC 里找到覆盖 `gid` 的 strike，再从 CBDT 里抽出它的 PNG 并解码。
///
/// # 实测出的结构（2020 版 Noto Color Emoji；换字体先对真字体验证）
///
/// ```text
/// CBLC+8+48*s          bitmapSizeTable（步长 48）：idxOff / idxLen / nsub / colorRef / …
/// CBLC+idxOff          index subtable **数组**：nsub 条，每条 8 字节：
///                        firstGlyph(u16) lastGlyph(u16) additionalOffset(u32)
/// CBLC+idxOff+addOff   indexSubTable 头：indexFormat(u16) imageFormat(u16) imageDataOffset(u32)
/// CBDT+imageDataOffset+codeOffset[i]    SmallGlyphMetrics(5B) + dataLen(4B) + PNG
/// ```
/// ⚠️ 三个偏移**各回各家**：idxOff / addOff 以 **CBLC** 为基准（索引住在 CBLC），
/// imageDataOffset / codeOffset 以 **CBDT** 为基准（位图住在 CBDT）。
/// 第一版实现把数组条目误读成 subtable 头（`00 04 00 11 00 00 00 18` 数字上恰好
/// 形似 `indexFmt=4, imageFmt=17`）—— 本注释就是那次的墓碑，别再踩。
fn strike_for_glyph(data: &[u8], t: Tables, gid: u32) -> Option<ColorStrike> {
    let (cblc_o, cblc_l) = t.cblc;
    let (cbdt_o, _cbdt_l) = t.cbdt;
    let nsizes = u32_at(data, cblc_o + 4)? as usize;
    for s in 0..nsizes {
        // bitmapSizeTable 步长 48（2020 版 Noto 实测；EBLC 老规范写的是别的数）。
        let b = cblc_o + 8 + 48 * s;
        if b + 48 > cblc_o + cblc_l {
            break;
        }
        let idx_off = u32_at(data, b)? as usize;
        let nsub = u32_at(data, b + 8)? as usize;
        let array = cblc_o + idx_off;
        if array + nsub * 8 > cblc_o + cblc_l {
            continue;
        }
        for i in 0..nsub {
            let e = array + 8 * i;
            let first = u16_at(data, e)? as u32;
            let last = u16_at(data, e + 2)? as u32;
            let add = u32_at(data, e + 4)? as usize;
            if gid < first || gid > last {
                continue;
            }
            let st = array + add;
            let ifmt = u16_at(data, st)?;
            let imgfmt = u16_at(data, st + 2)?;
            return match (ifmt, imgfmt) {
                (1, 17) => format1_png(data, st, cbdt_o, first, gid),
                _ => None, // 别的 index/image 格式：显式不支持，回落
            };
        }
    }
    None
}

/// indexFormat 1（`offsetArray[uint32]`，`last-first+2` 个）
/// + imageFormat 17（`SmallGlyphMetrics(5B) + dataLen(4B) + PNG`）。
///
/// metrics 里的尺寸/步进都是 **strike 自己的像素**（Noto：advance = strike 宽 = 1em）。
/// 调用方按 `advance_px` 的比例缩放到目标字号（见 `text_raster` 的合成段）。
fn format1_png(data: &[u8], st: usize, cbdt_o: usize, first: u32, gid: u32) -> Option<ColorStrike> {
    let idx = (gid - first) as usize;
    let arr = st + 8; // 头 8 字节（fmt/imgfmt/imageDataOffset）之后就是偏移数组
    let rel = u32_at(data, arr + 4 * idx)? as usize;
    // imageDataOffset / codeOffset 以 **CBDT** 为基准（位图住在 CBDT）。
    let idata = u32_at(data, st + 4)? as usize;
    let base = cbdt_o + idata + rel;
    let gh = *data.get(base)? as i32;
    let gw = *data.get(base + 1)? as i32;
    let bx = *data.get(base + 2)? as i32;
    let by = *data.get(base + 3)? as i32;
    let adv = *data.get(base + 4)? as i32;
    let dlen = u32_at(data, base + 5)? as usize;
    let png = data.get(base + 9..base + 9 + dlen)?;
    if png.len() < 8 || &png[0..4] != b"\x89PNG" {
        return None;
    }
    decode_png(png, gw, gh, bx, by, adv)
}

/// 解码 PNG（`png` crate 与 core 共用同一份 0.17），转直排 RGBA。
fn decode_png(png: &[u8], gw: i32, gh: i32, bx: i32, by: i32, adv: i32) -> Option<ColorStrike> {
    if gw <= 0 || gh <= 0 {
        return None;
    }
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    // ⚠️ Noto 的 strike PNG 是**调色板**格式（PLTE + tRNS）—— EXPAND 把它展开成
    // RGBA（tRNS 变 alpha）；没有这步，下面 match Indexed 会直接 None。
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    // 目标永远是 RGBA8（palette/灰度等彩色 emoji 字体不会用，但别 panic）
    let (w, h) = (info.width, info.height);
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    match info.color_type {
        png::ColorType::Rgba => {
            let src = &buf[..(w * h * 4) as usize];
            pixels.extend_from_slice(src);
        }
        png::ColorType::Rgb => {
            for px in buf[..(w * h * 3) as usize].chunks_exact(3) {
                pixels.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
        _ => return None,
    }
    Some(ColorStrike {
        width: w,
        height: h,
        pixels,
        bearing_x: bx,
        bearing_y: by,
        advance_px: adv.max(0) as u32,
    })
}

/// 目录里第一支**彩色位图字体**（CBDT+CBLC 都在）。按文件名排序取最前 —— 确定性。
///
/// 结果按目录缓存：切分与栅格化都会问，目录扫描不能留在热路径上。
pub(crate) fn color_font_in(dir: &Path) -> Option<PathBuf> {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<PathBuf, Option<PathBuf>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hit) = guard.get(dir) {
        return hit.clone();
    }
    let mut fonts: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc"))
                    .unwrap_or(false)
        })
        .collect();
    fonts.sort();
    let hit = fonts.into_iter().find(|p| has_color_bitmap(p));
    guard.insert(dir.to_path_buf(), hit.clone());
    hit
}

/// 解码结果缓存：同一 (字体, 码点) 只解一次 PNG（出片会反复问）。
pub fn color_strike_cached(path: &Path, codepoint: u32) -> Option<Arc<ColorStrike>> {
    static C: OnceLock<std::sync::Mutex<HashMap<(PathBuf, u32), Option<Arc<ColorStrike>>>>> =
        OnceLock::new();
    let cache = C.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hit) = guard.get(&(path.to_path_buf(), codepoint)) {
        return hit.clone();
    }
    let parsed = color_strike(path, codepoint).map(Arc::new);
    guard.insert((path.to_path_buf(), codepoint), parsed.clone());
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用户库里的这支 2020 版 Noto Color Emoji —— 本模块的全部偏移基准都是对它实测的。
    /// 没有这个文件（CI）时跳过。
    const FONT: &str = "/home/toki/project/yekitrim-release/data/global/resources/fonts/NotoColorEmoji-CBDT/font.ttf";

    fn font() -> Option<&'static Path> {
        let p = Path::new(FONT);
        p.is_file().then_some(p)
    }

    /// 表目录 + has_color_bitmap。
    #[test]
    fn detects_color_bitmap_font() {
        let Some(p) = font() else { return };
        assert!(has_color_bitmap(p), "CBDT+CBLC 都在 ⇒ 必须认成彩色位图字体");
        // 没有 CBDT 的字体要判否（用仓库里任何一支普通字体都行）
        let plain = Path::new("/tmp/fonts2/NotoSansSC-Regular.ttf");
        if plain.is_file() {
            assert!(!has_color_bitmap(plain), "普通 CJK 字体不该被认成彩色位图");
        }
    }

    /// 👮（U+1F46E，UTF-16 代理对之外的单个 astral 码点）必须抽出**彩色** PNG。
    #[test]
    fn extracts_the_police_strike_in_color() {
        let Some(p) = font() else { return };
        let s = color_strike(p, 0x1F46E).expect("👮 必须有 strike");
        assert_eq!((s.width, s.height), (136, 128), "Noto 2020 的 strike 尺寸");
        assert_eq!(s.advance_px, 136, "advance = strike 宽 = 1em");
        assert_eq!(s.bearing_y, 101, "位图顶边在基线上方 101px（Noto 实测）");
        // **彩色**：存在 R≠G≠B 的不透明像素（轮廓版不可能有）
        let colored = s.pixels.chunks_exact(4).filter(|px| px[3] > 10).any(|px| {
            let (mn, mx) = px[0..3]
                .iter()
                .fold((255u8, 0u8), |(a, b), &v| (a.min(v), b.max(v)));
            mx - mn > 40
        });
        assert!(colored, "strike 必须带彩色像素（这是 CBDT 的意义所在）");
    }

    /// 主字体覆盖的码点不能误取 strike（cmap format 12 的二分/线性扫要对）。
    #[test]
    fn missing_codepoints_return_none() {
        let Some(p) = font() else { return };
        assert!(color_strike(p, '中' as u32).is_none(), "CJK 不在彩色字体里");
        assert!(color_strike(p, 'A' as u32).is_none());
        assert!(color_strike(p, 0x10FFFF).is_none());
    }
}
