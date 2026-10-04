//! 最小 MP4 分离器：够喂 WebCodecs 就行。
//!
//! # 为什么自己写，而不是引 mp4box.js
//!
//! plan 当初写的是「MP4 用 mp4box.js」。这里改成自己写一个**够用**的解析器，理由三条：
//!
//!   1. 底座里 dhampir-media 已经有 Demuxer 契约，而实现是零——浏览器宿主这一侧正缺它。
//!      引一个 JS 库，等于把「帧号 → 样本」这条契约留在 JS 里，Rust 侧永远只是搬运工。
//!   2. 整数帧号是项目的铁律（不用浮点秒）。样本表、时间基、同步样本正是帧号的来源，
//!      必须在我们自己的类型上兑现，而不是透过一个 JS 对象的字段名。
//!   3. 不引第三方：本仓在依赖上一直克制，一个 500 KB 的 JS 库带进来还要配许可与版本对齐。
//!
//! # 它做什么、不做什么
//!
//! **做**：定位视频轨、读出 avcC（WebCodecs 的 description）、逐个样本的
//! (文件偏移, 字节数, 解码时间戳, 时长, 是否同步样本)。
//!
//! **不做**：编辑列表、碎片化 MP4(moof/mfra)、多轨合成、音频。
//! 碰到不认识的盒子就跳过——这是这个解析器唯一需要的健壮性。
//!
//! 这个模块**不在 cfg(wasm32) 之下**：它在 native 上照样编译与测试，
//! 于是「样本表算对了吗」这件事能在没有浏览器的机器上被验。

/// 一个样本（MP4 里的一帧编码数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// 在文件里的字节偏移。
    pub offset: usize,
    /// 字节数。
    pub size: usize,
    /// 解码时间戳，单位是轨的时间基。
    pub dts: u64,
    /// 时长，单位同上。
    pub duration: u32,
    /// 是否同步样本（关键帧）。WebCodecs 只允许从同步样本开始解码。
    pub is_sync: bool,
}

/// 一条视频轨。
#[derive(Debug, Clone)]
pub struct VideoTrack {
    /// 轨的时间基（每秒多少单位）。
    pub timescale: u32,
    /// 编码尺寸（像素）。
    pub width: u16,
    pub height: u16,
    /// avcC 盒的原始内容：WebCodecs 的 VideoDecoderConfig.description。
    pub description: Vec<u8>,
    /// 全部样本，按解码顺序。
    pub samples: Vec<Sample>,
}

impl VideoTrack {
    /// 每个样本时长是否都相同。
    ///
    /// **不做**帧号与时间戳的浮点换算：只有定帧率素材才谈得上「帧号 = 样本下标」。
    /// 变帧率素材的帧号得由时间线定义，不是由容器定义。
    pub fn is_constant_rate(&self) -> bool {
        match self.samples.first() {
            None => true,
            Some(first) => self.samples.iter().all(|s| s.duration == first.duration),
        }
    }

    /// 第 frame 帧的样本下标（0 基）。超出范围返回 None。
    pub fn sample_index_of_frame(&self, frame: usize) -> Option<usize> {
        if frame < self.samples.len() {
            Some(frame)
        } else {
            None
        }
    }

    /// 要解第 frame 帧，得从哪个同步样本开始（含）——WebCodecs 的硬约束。
    pub fn sync_sample_at_or_before(&self, frame: usize) -> Option<usize> {
        let last = self.samples.len().checked_sub(1)?;
        let upto = frame.min(last);
        (0..=upto).rev().find(|i| self.samples[*i].is_sync)
    }
}

/// 解析失败的原因。分成几类，好让报错能指到具体哪一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DemuxError {
    Truncated,
    NoMoov,
    NoVideoTrack,
    NoSampleDescription,
    NoAvcConfig,
    SampleOutOfRange,
}

impl core::fmt::Display for DemuxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            Self::Truncated => "文件被截断，盒子头读不满",
            Self::NoMoov => "找不到 moov",
            Self::NoVideoTrack => "moov 里没有视频轨",
            Self::NoSampleDescription => "视频轨里没有 stsd",
            Self::NoAvcConfig => "样本描述里没有 avcC",
            Self::SampleOutOfRange => "样本区间超出文件",
        };
        f.write_str(text)
    }
}

impl core::error::Error for DemuxError {}

/// 一个盒子的引用。
#[derive(Debug, Clone, Copy)]
struct BoxRef {
    kind: [u8; 4],
    /// 盒子内容（不含头）的起止。
    start: usize,
    end: usize,
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    let slice = data.get(at..at + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u64(data: &[u8], at: usize) -> Option<u64> {
    let slice = data.get(at..at + 8)?;
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(slice);
    Some(u64::from_be_bytes(bytes))
}

fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    let slice = data.get(at..at + 2)?;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

/// 遍历 [start, end) 里同一层的盒子。
fn boxes(data: &[u8], start: usize, end: usize) -> Vec<BoxRef> {
    let mut out = Vec::new();
    let mut at = start;
    while at + 8 <= end {
        let Some(size32) = read_u32(data, at) else {
            break;
        };
        let Some(kind_slice) = data.get(at + 4..at + 8) else {
            break;
        };
        let kind = [kind_slice[0], kind_slice[1], kind_slice[2], kind_slice[3]];
        let (header, size) = if size32 == 1 {
            match read_u64(data, at + 8) {
                Some(large) if large >= 16 => (16usize, large as usize),
                _ => break,
            }
        } else if size32 == 0 {
            (8usize, end - at)
        } else {
            (8usize, size32 as usize)
        };
        if size < header || at + size > end {
            break;
        }
        out.push(BoxRef {
            kind,
            start: at + header,
            end: at + size,
        });
        at += size;
    }
    out
}

fn find_box(list: &[BoxRef], kind: &[u8; 4]) -> Option<BoxRef> {
    list.iter().copied().find(|b| &b.kind == kind)
}

/// 从整份文件里解析出视频轨。
pub fn parse_video_track(bytes: &[u8]) -> Result<VideoTrack, DemuxError> {
    if bytes.len() < 8 {
        return Err(DemuxError::Truncated);
    }
    let top = boxes(bytes, 0, bytes.len());
    let moov = find_box(&top, b"moov").ok_or(DemuxError::NoMoov)?;

    for trak in boxes(bytes, moov.start, moov.end)
        .iter()
        .filter(|b| &b.kind == b"trak")
    {
        if let Some(track) = parse_trak(bytes, trak) {
            return Ok(track);
        }
    }
    Err(DemuxError::NoVideoTrack)
}

fn parse_trak(bytes: &[u8], trak: &BoxRef) -> Option<VideoTrack> {
    let children = boxes(bytes, trak.start, trak.end);
    let mdia = find_box(&children, b"mdia")?;
    let mdia_children = boxes(bytes, mdia.start, mdia.end);

    // hdlr：确认这是视频轨。音频轨与字幕轨在这里被跳过。
    let hdlr = find_box(&mdia_children, b"hdlr")?;
    let handler = read_u32(bytes, hdlr.start + 8)?;
    if &handler.to_be_bytes() != b"vide" {
        return None;
    }

    let mdhd = find_box(&mdia_children, b"mdhd")?;
    let version = *bytes.get(mdhd.start)?;
    // version 1 时 creation/modification/duration 都是 64 位
    let timescale = if version == 1 {
        read_u32(bytes, mdhd.start + 20)?
    } else {
        read_u32(bytes, mdhd.start + 12)?
    };

    let minf = find_box(&mdia_children, b"minf")?;
    let minf_children = boxes(bytes, minf.start, minf.end);
    let stbl = find_box(&minf_children, b"stbl")?;
    let stbl_children = boxes(bytes, stbl.start, stbl.end);

    let (width, height, description) = parse_stsd(bytes, &stbl_children)?;
    let samples = build_samples(bytes, &stbl_children)?;

    Some(VideoTrack {
        timescale,
        width,
        height,
        description,
        samples,
    })
}

/// 从 stsd 里取编码尺寸与 avcC。
fn parse_stsd(bytes: &[u8], stbl_children: &[BoxRef]) -> Option<(u16, u16, Vec<u8>)> {
    let stsd = find_box(stbl_children, b"stsd")?;
    let entry_count = read_u32(bytes, stsd.start + 4)?;
    if entry_count == 0 {
        return None;
    }
    let entries = boxes(bytes, stsd.start + 8, stsd.end);
    let entry = entries.first()?;
    // VisualSampleEntry：payload 偏移 24 = width、26 = height，子盒子从 78 开始。
    let width = read_u16(bytes, entry.start + 24)?;
    let height = read_u16(bytes, entry.start + 26)?;
    let children = boxes(bytes, entry.start + 78, entry.end);
    let avcc = find_box(&children, b"avcC")?;
    Some((width, height, bytes[avcc.start..avcc.end].to_vec()))
}

/// 由 stts / stsc / stsz / stco / stss 拼出样本表。
///
/// 这是整个解析器里唯一"要动脑子"的地方：MP4 不存每帧的绝对偏移，只存
/// 「每个 chunk 从哪开始」+「每个 chunk 装几个样本」+「每个样本多大」，
/// 偏移是一路加出来的。
fn build_samples(bytes: &[u8], stbl_children: &[BoxRef]) -> Option<Vec<Sample>> {
    // stsz：每张样本的字节数
    let stsz = find_box(stbl_children, b"stsz")?;
    let uniform_size = read_u32(bytes, stsz.start + 4)?;
    let count = read_u32(bytes, stsz.start + 8)? as usize;
    let mut sizes = Vec::with_capacity(count);
    if uniform_size != 0 {
        sizes.resize(count, uniform_size as usize);
    } else {
        for i in 0..count {
            sizes.push(read_u32(bytes, stsz.start + 12 + i * 4)? as usize);
        }
    }

    // stts：每张样本的时长（游程编码），顺便累出 dts
    let stts = find_box(stbl_children, b"stts")?;
    let stts_count = read_u32(bytes, stts.start + 4)? as usize;
    let mut durations: Vec<u32> = Vec::with_capacity(count);
    let mut dts_list: Vec<u64> = Vec::with_capacity(count);
    let mut dts = 0u64;
    for i in 0..stts_count {
        let base = stts.start + 8 + i * 8;
        let run = read_u32(bytes, base)? as usize;
        let delta = read_u32(bytes, base + 4)?;
        for _ in 0..run {
            durations.push(delta);
            dts_list.push(dts);
            dts += u64::from(delta);
        }
    }

    // stsc：chunk → 每 chunk 装几个样本（游程编码）
    let stsc = find_box(stbl_children, b"stsc")?;
    let stsc_count = read_u32(bytes, stsc.start + 4)? as usize;
    let mut chunk_runs: Vec<(u32, u32)> = Vec::with_capacity(stsc_count);
    for i in 0..stsc_count {
        let base = stsc.start + 8 + i * 12;
        let first_chunk = read_u32(bytes, base)?;
        let per_chunk = read_u32(bytes, base + 4)?;
        chunk_runs.push((first_chunk, per_chunk));
    }

    // stco / co64：每个 chunk 的文件偏移
    let chunk_offsets: Vec<u64> = if let Some(stco) = find_box(stbl_children, b"stco") {
        let n = read_u32(bytes, stco.start + 4)? as usize;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(u64::from(read_u32(bytes, stco.start + 8 + i * 4)?));
        }
        out
    } else {
        let co64 = find_box(stbl_children, b"co64")?;
        let n = read_u32(bytes, co64.start + 4)? as usize;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(read_u64(bytes, co64.start + 8 + i * 8)?);
        }
        out
    };

    // stss：哪些样本是同步样本（1 基）。没有这个盒子 = 全部同步。
    let sync: Vec<bool> = match find_box(stbl_children, b"stss") {
        Some(stss) => {
            let n = read_u32(bytes, stss.start + 4)? as usize;
            let mut flags = vec![false; count];
            for i in 0..n {
                let number = read_u32(bytes, stss.start + 8 + i * 4)? as usize;
                if number >= 1 && number <= count {
                    flags[number - 1] = true;
                }
            }
            flags
        }
        None => vec![true; count],
    };

    // 拼表：按 chunk 走，chunk 内样本连续排布。
    let mut samples = Vec::with_capacity(count);
    let mut index = 0usize;
    let chunk_count = chunk_offsets.len();
    // 用 `enumerate` 代替自己维护下标：`needless_range_loop` 要求的形状。
    // 语义不变 —— 下面用的还是 chunk 的序号与它自己的偏移。
    for (chunk_index, chunk_offset) in chunk_offsets.iter().enumerate().take(chunk_count) {
        let ordinal = (chunk_index + 1) as u32;
        // 该 chunk 每几个样本：取「first_chunk <= 本 chunk 序号」里最后一条
        let per_chunk = chunk_runs
            .iter()
            .rev()
            .find(|(first, _)| *first <= ordinal)
            .map(|(_, per)| *per)?;
        let mut offset = *chunk_offset;
        for _ in 0..per_chunk {
            if index >= count {
                break;
            }
            let size = sizes[index];
            let start = usize::try_from(offset).ok()?;
            if start + size > bytes.len() {
                return None;
            }
            samples.push(Sample {
                offset: start,
                size,
                dts: *dts_list.get(index)?,
                duration: *durations.get(index)?,
                is_sync: sync[index],
            });
            offset += size as u64;
            index += 1;
        }
    }
    if samples.len() != count {
        return None;
    }
    Some(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 手写一个够小的 MP4：不依赖任何素材文件，测试是自足的 ----

    fn be32(value: u32) -> [u8; 4] {
        value.to_be_bytes()
    }

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 8);
        out.extend_from_slice(&be32((payload.len() + 8) as u32));
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for p in parts {
            out.extend_from_slice(p);
        }
        out
    }

    /// 造一份最小 MP4：1 条 vide 轨、3 个样本（第 1 个是同步样本）。
    /// 三个样本在同一个 chunk 里，每个 4 字节。
    fn tiny_mp4() -> Vec<u8> {
        // avcC 内容随便给几个字节：解析器只负责原样带出去。
        let avcc = boxed(b"avcC", &[0x01, 0x42, 0x00, 0x1e, 0xff]);

        // VisualSampleEntry 的固定部分（78 字节）+ avcC
        let mut entry = Vec::new();
        entry.extend_from_slice(&[0u8; 6]); // reserved
        entry.extend_from_slice(&[0x00, 0x01]); // data_reference_index
        entry.extend_from_slice(&[0u8; 16]); // pre_defined + reserved + pre_defined[3]
        entry.extend_from_slice(&[0x02, 0x80]); // width = 640
        entry.extend_from_slice(&[0x01, 0x68]); // height = 360
        entry.extend_from_slice(&[0x00, 0x48, 0x00, 0x00]); // horizresolution
        entry.extend_from_slice(&[0x00, 0x48, 0x00, 0x00]); // vertresolution
        entry.extend_from_slice(&[0u8; 4]); // reserved
        entry.extend_from_slice(&[0x00, 0x01]); // frame_count
        entry.extend_from_slice(&[0u8; 32]); // compressorname
        entry.extend_from_slice(&[0x00, 0x18]); // depth
        entry.extend_from_slice(&[0xff, 0xff]); // pre_defined
        assert_eq!(
            entry.len(),
            78,
            "VisualSampleEntry 的固定部分必须是 78 字节"
        );
        entry.extend_from_slice(&avcc);

        let stsd = boxed(
            b"stsd",
            &concat(&[be32(0).to_vec(), be32(1).to_vec(), boxed(b"avc1", &entry)]),
        );

        // stts：3 个样本，每个 1000 tick
        let mut stts_body = concat(&[be32(0).to_vec(), be32(1).to_vec()]);
        stts_body.extend_from_slice(&be32(3));
        stts_body.extend_from_slice(&be32(1000));

        // stsc：第 1 个 chunk 起，每 chunk 3 个样本
        let mut stsc_body = concat(&[be32(0).to_vec(), be32(1).to_vec()]);
        stsc_body.extend_from_slice(&be32(1));
        stsc_body.extend_from_slice(&be32(3));
        stsc_body.extend_from_slice(&be32(1));

        // stsz：3 个样本各 4 字节
        let mut stsz_body = concat(&[be32(0).to_vec(), be32(0).to_vec(), be32(3).to_vec()]);
        for _ in 0..3 {
            stsz_body.extend_from_slice(&be32(4));
        }

        // stss：只有第 1 个样本是同步样本
        let mut stss_body = concat(&[be32(0).to_vec(), be32(1).to_vec()]);
        stss_body.extend_from_slice(&be32(1));

        // stco：一个 chunk，偏移先占位（最后回填）
        let mut stco_body = concat(&[be32(0).to_vec(), be32(1).to_vec()]);
        stco_body.extend_from_slice(&be32(0));

        let stbl = boxed(
            b"stbl",
            &concat(&[
                stsd,
                boxed(b"stts", &stts_body),
                boxed(b"stsc", &stsc_body),
                boxed(b"stsz", &stsz_body),
                boxed(b"stss", &stss_body),
                boxed(b"stco", &stco_body),
            ]),
        );

        // mdhd：version 0，timescale = 30000
        let mut mdhd_body = concat(&[be32(0).to_vec(), be32(0).to_vec(), be32(0).to_vec()]);
        mdhd_body.extend_from_slice(&be32(30000));
        mdhd_body.extend_from_slice(&be32(3000));
        mdhd_body.extend_from_slice(&be32(0));

        // hdlr：handler = vide
        let mut hdlr_body = concat(&[be32(0).to_vec(), be32(0).to_vec()]);
        hdlr_body.extend_from_slice(b"vide");
        hdlr_body.extend_from_slice(&[0u8; 12]);
        hdlr_body.push(0);

        let minf = boxed(b"minf", &stbl);
        let mdia = boxed(
            b"mdia",
            &concat(&[boxed(b"mdhd", &mdhd_body), boxed(b"hdlr", &hdlr_body), minf]),
        );
        let trak = boxed(b"trak", &mdia);
        let moov = boxed(b"moov", &trak);
        let ftyp = boxed(b"ftyp", b"isom");

        // mdat 载荷起点 = ftyp + moov + 8（mdat 自己的头）
        let mdat_payload_start = ftyp.len() + moov.len() + 8;

        // 回填 stco 里那个占位偏移：stco 盒子头 4 字节 + 类型 4 字节 → payload，
        // 再 +8 到 offset 字段。所以字段位置 = 类型标记位置 + 12。
        let mut moov_patched = moov.clone();
        let marker = moov_patched
            .windows(4)
            .position(|w| w == b"stco")
            .expect("moov 里应当有 stco");
        let field = marker + 12;
        moov_patched[field..field + 4].copy_from_slice(&be32(mdat_payload_start as u32));

        concat(&[ftyp, moov_patched, boxed(b"mdat", &[0u8; 12])])
    }

    #[test]
    fn 解析出三条样本与同步样本标记() {
        let file = tiny_mp4();
        let track = parse_video_track(&file).expect("应当解析成功");
        assert_eq!(track.timescale, 30000);
        assert_eq!(track.width, 640);
        assert_eq!(track.height, 360);
        assert_eq!(track.samples.len(), 3);
        assert_eq!(track.description, vec![0x01, 0x42, 0x00, 0x1e, 0xff]);
        assert!(track.samples[0].is_sync);
        assert!(!track.samples[1].is_sync);
        assert!(!track.samples[2].is_sync);
    }

    #[test]
    fn 样本偏移与时长按样本表算出来() {
        let file = tiny_mp4();
        let track = parse_video_track(&file).expect("应当解析成功");
        assert_eq!(track.samples[1].offset, track.samples[0].offset + 4);
        assert_eq!(track.samples[2].offset, track.samples[0].offset + 8);
        assert_eq!(track.samples[0].size, 4);
        assert_eq!(track.samples[0].dts, 0);
        assert_eq!(track.samples[1].dts, 1000);
        assert_eq!(track.samples[2].dts, 2000);
        assert_eq!(track.samples[0].duration, 1000);
        assert!(track.samples[2].offset + 4 <= file.len());
    }

    #[test]
    fn 帧号到样本下标与回溯同步样本() {
        let file = tiny_mp4();
        let track = parse_video_track(&file).expect("应当解析成功");
        assert_eq!(track.sample_index_of_frame(2), Some(2));
        assert_eq!(track.sample_index_of_frame(3), None);
        assert_eq!(track.sync_sample_at_or_before(2), Some(0));
        assert_eq!(track.sync_sample_at_or_before(0), Some(0));
    }

    #[test]
    fn 定帧率判定() {
        let file = tiny_mp4();
        let track = parse_video_track(&file).expect("应当解析成功");
        assert!(track.is_constant_rate());
    }

    /// 拿真 proxy 验一遍：样本表必须与 ffprobe 独立量出来的事实逐项对上。
    ///
    /// 素材是 target/ 下的生成物（不入库），所以这条默认 #[ignore]：
    ///   ffmpeg -y -f lavfi -i testsrc2=size=1920x1080:rate=60 -t 8 -c:v libx264 ...
    ///   ffmpeg -y -i target/s3/source1080p.mp4 -vf scale=-2:720 -c:v libx264 -g 60 -keyint_min 60 -sc_threshold 0 -crf 23 -an target/s3/proxy720p.mp4
    /// 跑：cargo test -p dhampir-wasm demux -- --ignored
    ///
    /// 期望值不是从这份代码里抄的，是先前的实测：probe 报 480 帧、1280x720、
    /// 关键帧 8 个且间隔恒为 60（见 plan/s3.2-proxy-spec.md 第 3 节）。
    #[test]
    #[ignore = "需要 target/s3/proxy720p.mp4；生成命令见这条测试的文档注释"]
    fn 真素材的样本表与先前实测对上() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/s3/proxy720p.mp4");
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => panic!("读不到 {path}：{e}（先生成 proxy）"),
        };
        let track = parse_video_track(&bytes).expect("真 proxy 应当能解析");

        assert_eq!(track.width, 1280, "编码宽度应当与 ffprobe 报的一致");
        assert_eq!(track.height, 720);
        assert_eq!(track.samples.len(), 480, "8 秒 60fps = 480 帧");
        assert!(track.is_constant_rate(), "这条 proxy 是定帧率");

        // 样本必须全部落在文件里，而且区间互不重叠、首尾相接
        let mut expected_offset = track.samples[0].offset;
        for (i, s) in track.samples.iter().enumerate() {
            assert!(
                s.offset + s.size <= bytes.len(),
                "第 {i} 个样本越过文件末尾"
            );
            assert_eq!(
                s.offset, expected_offset,
                "第 {i} 个样本的偏移应当紧接前一个"
            );
            expected_offset += s.size;
        }

        // 关键帧：先前实测是 8 个、间隔恒为 60
        let sync: Vec<usize> = track
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_sync)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(sync.len(), 8, "先前实测：8 个关键帧");
        assert_eq!(sync, vec![0, 60, 120, 180, 240, 300, 360, 420]);

        // 时间基与帧时长要和 60fps 自洽
        assert_eq!(track.timescale % 60, 0, "时间基应当能被 60 整除");
        assert_eq!(
            track.samples[0].duration,
            track.timescale / 60,
            "每帧时长 = 时间基 / 60"
        );
        assert_eq!(track.samples[1].dts, u64::from(track.samples[0].duration));
    }

    #[test]
    fn 截断与缺盒子会报出来() {
        // 用 matches! 而不是 assert_eq!：VideoTrack 不需要为了测试去实现 PartialEq。
        assert!(matches!(parse_video_track(&[]), Err(DemuxError::Truncated)));
        assert!(matches!(
            parse_video_track(&[0, 0, 0, 8, b'f', b't', b'y', b'p']),
            Err(DemuxError::NoMoov)
        ));
        let mut broken = tiny_mp4();
        let at = broken.windows(4).position(|w| w == b"moov").unwrap();
        broken[at..at + 4].copy_from_slice(b"xxxx");
        assert!(matches!(
            parse_video_track(&broken),
            Err(DemuxError::NoMoov)
        ));
    }

    #[test]
    fn 真实解析出的样本表能切成首尾相接的_gop_段() {
        // 这条是 P4.1 的**端到端数据断言**：用的不是手搭的样本数组，
        // 而是走真正的 MP4 解析器 —— 从字节到样本表到 GOP 段。
        // 它比纯粹测 gop_slices 强的地方在于：样本表本身也是被解析出来的，
        // 只有解析与切片**两头都对**，这条才过得去。
        use dhampir_core::timeline::host_api::{SampleView, gop_slices};

        let track = parse_video_track(&tiny_mp4()).expect("应当能解析");
        let samples: Vec<SampleView> = track
            .samples
            .iter()
            .map(|s| SampleView {
                offset: s.offset,
                size: s.size,
                dts: s.dts,
                duration: s.duration,
                is_sync: s.is_sync,
            })
            .collect();
        let slices = gop_slices(&samples);

        let sync_count = samples.iter().filter(|s| s.is_sync).count();
        assert!(sync_count > 0, "夹具里应当有关键帧，否则这条测试是空转");
        assert_eq!(slices.len(), sync_count, "段数应当等于同步样本数");

        // 每一段的第一个样本**必须**是同步样本 ——
        // 这是「起点是关键帧」的直接证据，也是前端能不能解出第一帧的前提。
        for slice in &slices {
            let head = &samples[slice.first_sample as usize];
            assert!(head.is_sync, "第 {} 段的起点不是关键帧", slice.index);
        }

        // 首尾相接：有空洞会让前端少一帧，重叠会让同一帧被取两次。
        for pair in slices.windows(2) {
            assert_eq!(
                pair[0].first_sample + pair[0].sample_count,
                pair[1].first_sample,
                "第 {} 段与下一段不相接",
                pair[0].index
            );
        }

        // 最后一段一直延伸到样本表末尾（末段的关键帧之后没有下一个关键帧）。
        let last = slices.last().expect("至少有一段");
        assert_eq!(last.first_sample + last.sample_count, samples.len() as u32);
    }
}
