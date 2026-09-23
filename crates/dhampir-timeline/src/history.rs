//! 文档历史层：**整份快照**的撤销栈。
//!
//! # 为什么是快照，不是「反向 op」
//!
//! `remove` / `split` / `trim` 都是**有损**的 —— 被删掉的区间、切之前那份 `keyframes`
//! 都回不来，想「反过来执行一遍」是推不出来的。而 `ProjectDoc` 是**可序列化的纯数据**，
//! 一份克隆就换一个「一定能回到原样」，这正是验收要的那句「**逐字节相同**」。
//!
//! # 两份调用方共用这一份
//!
//! CLI（`dhampir edit --history`）与预览（wasm 宿主）调的是**同一个类型、同一套规则**；
//! 谁都不许在宿主里再写一份「差不多的撤销」。
//!
//! # 上限
//!
//! `cap` 按**条数**算，到顶丢**最旧**的一条 —— 编辑不该因为「历史满了」而失败。

use crate::project::ProjectDoc;

/// 一条历史记录：**改动之前**的整份文档 + 一句话说明。
///
/// 说明是**人读的**（预览里显示「撤销：剃刀」），所以它描述的是「这一压栈对应了哪一步编辑」。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub label: String,
    pub doc: ProjectDoc,
}

/// 线性撤销栈。两个栈对推：`past` 里放「可以退回去的状态」，`future` 里放「可以再前进回去的状态」。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct History {
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
    cap: usize,
    /// 上一次压栈用的**合并键**（只有 `push_coalescing` 会设它）。
    ///
    /// 一次拖拽会生成一长串 `move`，撤销应当回到**拖拽之前**而不是往回挪一帧 ——
    /// 合并靠的就是「后来这一次带的键与上一次相同」。
    #[serde(default)]
    last_key: Option<String>,
}

impl History {
    /// `cap` 是**条数**上限。给 0 当 1 处理：上限为零的历史层不是一个有用的东西。
    pub fn new(cap: usize) -> Self {
        Self {
            past: Vec::new(),
            future: Vec::new(),
            cap: cap.max(1),
            last_key: None,
        }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    /// 还能往回退几步。
    pub fn depth(&self) -> usize {
        self.past.len()
    }

    /// 还能再前进几步。
    pub fn redo_depth(&self) -> usize {
        self.future.len()
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    /// 全清（换一份工程就是换一条历史）。
    pub fn reset(&mut self) {
        self.past.clear();
        self.future.clear();
        self.last_key = None;
    }

    /// 记一步：`before` 是**改动前**的整份文档。这一步之后重做栈作废（标准语义）。
    pub fn push(&mut self, label: impl Into<String>, before: ProjectDoc) {
        self.last_key = None;
        self.record(label.into(), before);
    }

    /// 记一步，但**带上一个合并键**：键与上一次相同、且已经有东西可退时不重复压栈。
    ///
    /// 返回「有没有真的压」——调用方要据此决定是不是该把历史落盘。
    pub fn push_coalescing(
        &mut self,
        label: impl Into<String>,
        before: ProjectDoc,
        key: &str,
    ) -> bool {
        if self.last_key.as_deref() == Some(key) && !self.past.is_empty() {
            return false;
        }
        self.record(label.into(), before);
        self.last_key = Some(key.to_string());
        true
    }

    fn record(&mut self, label: String, before: ProjectDoc) {
        self.future.clear();
        self.past.push(Snapshot { label, doc: before });
        // 到顶丢最旧的那条：丢的是**最远的过去**，不是刚记下的这一步。
        while self.past.len() > self.cap {
            self.past.remove(0);
        }
    }

    /// 退一步。`current` 是**现在这份**文档。
    ///
    /// 没有可退的就返回 `None`（**不改任何东西** —— 连重做栈也不动）。
    ///
    /// 压进重做栈的那条，说明沿用**被退掉的那一步**的名字：重做之后回到的正是它，
    /// 所以 `redo` 拿回来的说明也是同一步，而不是「将要做什么」。
    pub fn undo(&mut self, current: ProjectDoc) -> Option<Snapshot> {
        let restored = self.past.pop()?;
        self.future.push(Snapshot { label: restored.label.clone(), doc: current });
        self.last_key = None;
        Some(restored)
    }

    /// 进一步。语义与 [`History::undo`] 对称。
    pub fn redo(&mut self, current: ProjectDoc) -> Option<Snapshot> {
        let restored = self.future.pop()?;
        self.past.push(Snapshot { label: restored.label.clone(), doc: current });
        self.last_key = None;
        Some(restored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::{LAYER_SCHEMA_VERSION, TimelineV2};
    use crate::project::shell_from_timeline;
    use crate::schema::TimebaseDto;

    /// 只用来当「一份文档」使：历史层不认识时间线内容，所以给个空的就够。
    fn doc(fps: u32) -> ProjectDoc {
        shell_from_timeline(TimelineV2 {
            schema: LAYER_SCHEMA_VERSION,
            timebase: TimebaseDto { num: fps, den: 1 },
            markers: Vec::new(),
            tracks: Vec::new(),
        })
    }

    #[test]
    fn 退一步拿回来的必须是当初那一份() {
        let before = doc(30);
        let mut history = History::new(8);
        history.push("剃刀", before.clone());
        let after = doc(60);
        let restored = history.undo(after.clone()).expect("应当有可退的");
        assert_eq!(restored.doc, before, "undo 给的必须是当初压进去的那一份");
        assert_eq!(restored.label, "剃刀");
        assert_eq!(history.depth(), 0);
        assert_eq!(history.redo_depth(), 1);

        let forward = history.redo(before).expect("应当有可重做的");
        assert_eq!(forward.doc, after, "redo 给回的必须是刚才退掉的那一份");
        assert_eq!(history.depth(), 1);
    }

    #[test]
    fn 空历史退不了也不许改东西() {
        let mut history = History::new(4);
        assert!(!history.can_undo());
        assert!(history.undo(doc(30)).is_none());
        assert!(history.redo(doc(30)).is_none());
        assert_eq!(history.depth(), 0);
        assert_eq!(history.redo_depth(), 0);
    }

    #[test]
    fn 记了新的一步之后重做就作废() {
        let mut history = History::new(8);
        history.push("第一步", doc(30));
        let undone = history.undo(doc(60)).expect("有可退的");
        assert!(history.can_redo());
        history.push("岔路", undone.doc);
        assert!(!history.can_redo(), "新编辑之后重做栈必须作废");
        assert_eq!(history.depth(), 1);
    }

    #[test]
    fn 到顶丢最旧的一条而不是拒绝新的() {
        let mut history = History::new(2);
        history.push("1", doc(1));
        history.push("2", doc(2));
        history.push("3", doc(3));
        assert_eq!(history.depth(), 2, "上限是 2 条");
        // 退两次：拿到 3、2 —— **1 已经被挤出去了**。
        assert_eq!(history.undo(doc(4)).unwrap().doc, doc(3));
        assert_eq!(history.undo(doc(5)).unwrap().doc, doc(2));
        assert!(!history.can_undo(), "第 1 条已被挤出去，不该还能退");
    }

    #[test]
    fn 上限给零当一处理() {
        let mut history = History::new(0);
        history.push("一", doc(1));
        history.push("二", doc(2));
        assert_eq!(history.cap(), 1);
        assert_eq!(history.depth(), 1);
    }

    #[test]
    fn 合并键相同的连续操作只算一步() {
        let before_drag = doc(30);
        let mut history = History::new(8);
        // 拖拽：第一次压，后面几次同键不压。
        assert!(history.push_coalescing("拖动 c", before_drag.clone(), "move:c"));
        assert!(!history.push_coalescing("拖动 c", doc(31), "move:c"));
        assert!(!history.push_coalescing("拖动 c", doc(32), "move:c"));
        assert_eq!(history.depth(), 1, "一次拖拽只该留一步");
        // 撤销必须回到**拖拽之前**，不是往回挪一帧。
        assert_eq!(history.undo(doc(33)).unwrap().doc, before_drag);
    }

    #[test]
    fn 换了合并键就是新的一步() {
        let mut history = History::new(8);
        assert!(history.push_coalescing("拖 a", doc(1), "move:a"));
        assert!(history.push_coalescing("拖 b", doc(2), "move:b"), "换了元素就是新的一步");
        assert!(history.push_coalescing("拖 a", doc(3), "move:a"), "中间隔了别的键，必须重新压");
        assert_eq!(history.depth(), 3);
    }

    #[test]
    fn 不带合并键的压栈会把合并键清掉() {
        let mut history = History::new(8);
        assert!(history.push_coalescing("拖 a", doc(1), "move:a"));
        history.push("剃刀", doc(2));
        assert!(
            history.push_coalescing("拖 a", doc(3), "move:a"),
            "中间夹了一步别的编辑，同样的合并键也必须重新压",
        );
        assert_eq!(history.depth(), 3);
    }

    #[test]
    fn 历史本身能序列化着走一圈() {
        // CLI 要把历史落盘再读回来，所以这条是**功能**，不是好看。
        let mut history = History::new(8);
        history.push_coalescing("拖 a", doc(30), "move:a");
        history.push("剃刀", doc(60));
        history.undo(doc(90));
        let text = serde_json::to_string(&history).expect("历史要能序列化");
        let back: History = serde_json::from_str(&text).expect("历史要能读回来");
        assert_eq!(back, history);
        assert_eq!(back.depth(), 1);
        assert_eq!(back.redo_depth(), 1);
    }
}
