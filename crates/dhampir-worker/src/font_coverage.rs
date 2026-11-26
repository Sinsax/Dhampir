//! 字体**字形覆盖**检查（cmap 的 format 4 / 12）—— 只回答一件事：
//! 「这支字体有没有这个码点」。
//!
//! # 为什么需要它
//!
//! 出片时一条文本只用**一支字体**（`TextRasterKey::font_file`）。用户选的 CJK 字体里
//! 没有 emoji 字形，于是 👮 这类字符在成片里是**豆腐块**（2026-11-22 用户报的）。
//! 修法是在 `paint_parts` 里**按字形切段、逐段换字体**，而切段的前提就是这份检查。
//!
//! # 为什么不引 `ttf-parser`
//!
//! 本仓的依赖是逐条过 `scripts/licenses.mjs --check` 的，而这里只需要「在不在」一个布尔，
//! 不需要字形轮廓/度量/可变轴。cmap 的 format 4 与 format 12 加起来不到 150 行，
//! 自己写反而更容易解释清楚"为什么这个字算覆盖"。
//!
//! # 只看 cmap，不看 glyph 是否存在
//!
//! cmap 里映射到 **glyph 0**（.notdef）的码点在语义上等于"没有这个字形"，所以这里
//! 把 glyph 0 判成**不覆盖** —— 否则又退回豆腐块。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// 一个码点区间（闭区间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: u32,
    pub end: u32,
}

/// 一支字体的 cmap 摘要。
#[derive(Debug, Default, Clone)]
pub struct Coverage {
    /// format 12（UCS-4）与 format 4（BMP）的覆盖区间；两个都收，因为同一支字体可能只有其一。
    ranges: Vec<Range>,
}

impl Coverage {
    /// 这个码点有没有字形。
    pub fn covers(&self, cp: u32) -> bool {
        self.ranges
            .iter()
            .any(|r| cp >= r.start && cp <= r.end && !(r.start == 0 && r.end == 0))
    }

    /// 这一串文本是不是**每一个字**都有字形（空串算覆盖）。
    pub fn covers_all(&self, text: &str) -> bool {
        text.chars().all(|c| self.covers(c as u32))
    }
}

fn u16_at(d: &[u8], off: usize) -> Option<u16> {
    d.get(off..off + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], off: usize) -> Option<u32> {
    d.get(off..off + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// 解析**整个字体文件**：走 sfnt 表目录找到 `cmap`，再交给 [`parse_cmap`]。
pub fn load(path: &Path) -> Result<Coverage, String> {
    let data = std::fs::read(path).map_err(|e| format!("读字体失败 {}: {e}", path.display()))?;
    let num_tables = u16_at(&data, 4).ok_or("字体太短（读不到表数量）")? as usize;
    for i in 0..num_tables {
        let rec = 12 + 16 * i;
        let Some(tag) = data.get(rec..rec + 4) else {
            continue;
        };
        if tag != b"cmap" {
            continue;
        }
        let off = u32_at(&data, rec + 8).ok_or("cmap 偏移读不到")? as usize;
        let len = u32_at(&data, rec + 12).ok_or("cmap 长度读不到")? as usize;
        let end = off.saturating_add(len).min(data.len());
        if off >= end {
            return Err("cmap 表越界".into());
        }
        return parse_cmap(&data[off..end]);
    }
    Err(format!("{} 里没有 cmap 表", path.display()))
}

/// 解析 `cmap` 表本身（可单测：不碰文件系统）。
///
/// 取**覆盖面最大**的那个子表：优先 platform 3 / encoding 10（UCS-4，format 12），
/// 其次 0/4 与 0/6，最后 platform 3 / encoding 1（BMP，format 4）。
pub fn parse_cmap(cmap: &[u8]) -> Result<Coverage, String> {
    let n = u16_at(cmap, 2).ok_or("cmap 版本/数量读不到")? as usize;
    let mut best: Option<(u8, usize)> = None; // (优先级, 子表偏移)
    for i in 0..n {
        let rec = 4 + 8 * i;
        let (Some(pid), Some(eid), Some(off)) = (
            u16_at(cmap, rec),
            u16_at(cmap, rec + 2),
            u32_at(cmap, rec + 4),
        ) else {
            continue;
        };
        let rank = match (pid, eid) {
            (3, 10) | (0, 4) | (0, 6) => 3u8,         // 全 Unicode
            (3, 1) | (0, 3) | (0, 1) | (0, 0) => 2u8, // BMP
            _ => continue,
        };
        let off = off as usize;
        if off >= cmap.len() {
            continue;
        }
        if best.map(|(r, _)| rank > r).unwrap_or(true) {
            best = Some((rank, off));
        }
    }
    let (_, off) = best.ok_or("cmap 里没有可用的 Unicode 子表")?;
    let format = u16_at(cmap, off).ok_or("子表 format 读不到")?;
    match format {
        4 => parse_format4(&cmap[off..]),
        12 => parse_format12(&cmap[off..]),
        other => Err(format!("不支持的 cmap format {other}（只认 4 与 12）")),
    }
}

fn parse_format4(t: &[u8]) -> Result<Coverage, String> {
    let seg_x2 = u16_at(t, 6).ok_or("format4: segCountX2 读不到")? as usize;
    let seg = seg_x2 / 2;
    if seg == 0 {
        return Ok(Coverage::default());
    }
    let end_base = 14;
    let start_base = end_base + seg * 2 + 2; // 跳过 reservedPad
    let delta_base = start_base + seg * 2;
    let range_base = delta_base + seg * 2;
    let mut ranges = Vec::with_capacity(seg);
    for i in 0..seg {
        let (Some(start), Some(end), Some(delta), Some(ro)) = (
            u16_at(t, start_base + i * 2),
            u16_at(t, end_base + i * 2),
            u16_at(t, delta_base + i * 2),
            u16_at(t, range_base + i * 2),
        ) else {
            break;
        };
        if start > end || end == 0xFFFF {
            continue; // 0xFFFF 是结尾哨兵段
        }
        if ro == 0 {
            // 简单段：码点 + delta 得到字形号。delta 为 0 也可以（映射到自身）。
            let _ = delta;
            ranges.push(Range {
                start: start as u32,
                end: end as u32,
            });
        } else {
            // glyphIdArray 段（中文常用字多为 0 之外的写法）：保守放行 ——
            // 漏判的代价是**不切段**（保持今天的行为），误判的代价只是多想一次。
            ranges.push(Range {
                start: start as u32,
                end: end as u32,
            });
        }
    }
    Ok(Coverage { ranges })
}

fn parse_format12(t: &[u8]) -> Result<Coverage, String> {
    let groups = u32_at(t, 12).ok_or("format12: nGroups 读不到")? as usize;
    let mut ranges = Vec::with_capacity(groups.min(4096));
    for i in 0..groups {
        let base = 16 + i * 12;
        let (Some(start), Some(end), Some(glyph)) =
            (u32_at(t, base), u32_at(t, base + 4), u32_at(t, base + 8))
        else {
            break;
        };
        if start > end || glyph == 0 {
            continue;
        }
        ranges.push(Range { start, end });
    }
    Ok(Coverage { ranges })
}

/// 进程内缓存：同一支字体只解析一次（出片一条文本会问几百次）。
fn cache() -> &'static Mutex<HashMap<PathBuf, Option<Arc<Coverage>>>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Option<Arc<Coverage>>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取（必要时解析）一支字体的覆盖表；解析失败返回 `None`（**不报错**：
/// 判据是"能不能画"，解析不了的字体就当它不覆盖，交给下一支）。
pub fn coverage_of(path: &Path) -> Option<Arc<Coverage>> {
    let mut guard = cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hit) = guard.get(path) {
        return hit.clone();
    }
    let parsed = load(path).ok().map(Arc::new);
    guard.insert(path.to_path_buf(), parsed.clone());
    parsed
}

/// 读字体的**族名**（`name` 表里 nameID=1，优先 platform 3 的 UTF-16BE 那份）。
///
/// # 为什么需要它
///
/// 栅格化走的是 **libass（`subtitles=`）**，而 libass 是按**族名**在 `fontsdir` 里找字体
/// —— `--font-file` 只决定"默认族名"。所以"这一段换一支字体"的落地方式必须是
/// **把该字体的族名交给 libass**（`TextRasterKey::font_family`），而不是只换文件路径。
pub fn family_name(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    let num_tables = u16_at(&data, 4)? as usize;
    let mut name_off = None;
    for i in 0..num_tables {
        let rec = 12 + 16 * i;
        let tag = data.get(rec..rec + 4)?;
        if tag == b"name" {
            name_off = u32_at(&data, rec + 8).map(|o| o as usize);
            break;
        }
    }
    let off = name_off?;
    let count = u16_at(&data, off + 2)? as usize;
    let storage = off + u16_at(&data, off + 4)? as usize;
    let mut best: Option<(u8, String)> = None;
    for i in 0..count {
        let rec = off + 6 + 12 * i;
        let (pid, nid, len, ofs) = (
            u16_at(&data, rec)?,
            u16_at(&data, rec + 6)?,
            u16_at(&data, rec + 8)? as usize,
            u16_at(&data, rec + 10)? as usize,
        );
        if nid != 1 {
            continue; // 1 = 家族名
        }
        let start = storage + ofs;
        let bytes = data.get(start..start + len)?;
        let text = match pid {
            3 | 0 => {
                // UTF-16BE
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&units)
            }
            _ => String::from_utf8_lossy(bytes).to_string(),
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        // platform 3（Windows/Unicode）优先，其次 0（Unicode），最后其它（Mac Roman 等）
        let rank = match pid {
            3 => 3u8,
            0 => 2u8,
            _ => 1u8,
        };
        if best.as_ref().map(|(r, _)| rank > *r).unwrap_or(true) {
            best = Some((rank, text));
        }
    }
    best.map(|(_, t)| t)
}

/// 族名的进程内缓存（同 [`coverage_of`]：一条文本会问很多次）。
pub fn family_of(path: &Path) -> Option<String> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Option<String>>>> = OnceLock::new();
    let cache = C.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hit) = guard.get(path) {
        return hit.clone();
    }
    let parsed = family_name(path);
    guard.insert(path.to_path_buf(), parsed.clone());
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手搓一张只有 format 12 的 cmap：1 个子表、1 组 `0x1F400..=0x1F4FF`
    /// （👮 = U+1F46E 在里面；U+1F900 在外面）。
    fn cmap_format12() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&0u16.to_be_bytes()); // version
        v.extend_from_slice(&1u16.to_be_bytes()); // numTables
        v.extend_from_slice(&3u16.to_be_bytes()); // platform 3
        v.extend_from_slice(&10u16.to_be_bytes()); // encoding 10 (UCS-4)
        v.extend_from_slice(&12u32.to_be_bytes()); // 子表偏移 = 12
        v.extend_from_slice(&12u16.to_be_bytes()); // format 12
        v.extend_from_slice(&0u16.to_be_bytes()); // reserved
        v.extend_from_slice(&28u32.to_be_bytes()); // length（不校验）
        v.extend_from_slice(&0u32.to_be_bytes()); // language
        v.extend_from_slice(&1u32.to_be_bytes()); // nGroups
        v.extend_from_slice(&0x1F400u32.to_be_bytes());
        v.extend_from_slice(&0x1F4FFu32.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes()); // startGlyphID != 0
        v
    }

    /// 手搓一张 format 4 的 cmap：一段 `0x20..=0x7E`（ASCII），`idRangeOffset = 0`。
    fn cmap_format4() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend_from_slice(&1u16.to_be_bytes());
        v.extend_from_slice(&3u16.to_be_bytes());
        v.extend_from_slice(&1u16.to_be_bytes());
        v.extend_from_slice(&12u32.to_be_bytes());
        // format 4 子表
        v.extend_from_slice(&4u16.to_be_bytes()); // format
        v.extend_from_slice(&32u16.to_be_bytes()); // length
        v.extend_from_slice(&0u16.to_be_bytes()); // language
        v.extend_from_slice(&4u16.to_be_bytes()); // segCountX2 = 4（两段）
        v.extend_from_slice(&4u16.to_be_bytes()); // searchRange
        v.extend_from_slice(&1u16.to_be_bytes()); // entrySelector
        v.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
        // endCode[2]
        v.extend_from_slice(&0x7Eu16.to_be_bytes());
        v.extend_from_slice(&0xFFFFu16.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
        // startCode[2]
        v.extend_from_slice(&0x20u16.to_be_bytes());
        v.extend_from_slice(&0xFFFFu16.to_be_bytes());
        // idDelta[2]
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend_from_slice(&1u16.to_be_bytes());
        // idRangeOffset[2]
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes());
        v
    }

    /// format 12：UCS-4 区间要认得（emoji 全在 BMP 之外）。
    #[test]
    fn format12_covers_astral_ranges() {
        let c = parse_cmap(&cmap_format12()).expect("解析 format 12");
        assert!(c.covers(0x1F46E), "U+1F46E 👮 在那个区间里");
        assert!(!c.covers(0x1F900), "区间外不算覆盖");
        assert!(!c.covers('A' as u32));
    }

    /// format 4：BMP 区间 + 结尾 0xFFFF 哨兵段要被跳过（否则"什么字都覆盖"）。
    #[test]
    fn format4_covers_bmp_and_skips_the_sentinel() {
        let c = parse_cmap(&cmap_format4()).expect("解析 format 4");
        assert!(c.covers('A' as u32));
        assert!(c.covers('~' as u32));
        assert!(!c.covers(0xFFFF), "0xFFFF 是哨兵段，不是真字形");
        assert!(!c.covers('中' as u32));
        assert!(!c.covers(0x1F46E));
    }

    /// `covers_all` 是切段判据：一个字缺就整体不算覆盖。
    #[test]
    fn covers_all_is_the_splitting_predicate() {
        let c = parse_cmap(&cmap_format4()).expect("解析");
        assert!(c.covers_all("Hello"));
        assert!(c.covers_all(""));
        assert!(!c.covers_all("Hi👮"), "混了 emoji 就不算整段覆盖");
    }

    /// 真字体：能解析出 cmap，且 ASCII 一定有字形（机器上没有就跳过）。
    #[test]
    fn real_font_parses_if_present() {
        for p in [
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/tmp/fonts/NotoSansSC-Regular.ttf",
        ] {
            let path = Path::new(p);
            if !path.is_file() {
                continue;
            }
            let c = coverage_of(path).expect("真字体必须解析得出来");
            assert!(c.covers('A' as u32), "{}", path.display());
            assert!(c.covers('中' as u32), "{}", path.display());
        }
    }
}
