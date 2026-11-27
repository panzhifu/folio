//! 图文叠的判定族：一单元一图、图注在不在图前。单开一间是因为 `collect.rs`
//! 贴着行数基线；判定口径的理由跟着方法走，消费方（`stack_run` / `children`）经
//! 同一类型的方法调用，路径不变。

//! 图文叠的判定族：一单元一图、图注在不在图前。单开一间是因为 `collect.rs`
//! 贴着行数基线；判定口径的理由跟着方法走，消费方（`stack_run` / `children`）经
//! 同一类型的方法调用，路径不变。

use folio_format::dom::{NodeId, NodeKind};

use super::{Ctx, STACK_CAPTION_MAX, has_image, single_image, text_len_in};

impl<'a> Ctx<'a> {
    /// 这一块是不是一个「图单元」：恰好一张图，外加最多一段**短图注**。
    ///
    /// 与 [`single_image`] 的分别是后者只认「一个字的字都没有」那一档，而多看/掌阅那族
    /// 导出的画廊 cell 长这样（《诡秘之主》ffwp000.xhtml「诡秘地理日志」连着九个）：
    ///
    /// ```html
    /// <div class="duokan-image-gallery-cell">
    ///   <img src="../Images/xm001.jpg"/><p class="tpcrb"><b>灾祸之城</b></p>
    /// </div>
    /// ```
    ///
    /// 图旁边站着一段注，`single_image` 当场回 `None` —— 这本书的画廊因此从来没叠过，
    /// 一张注一块地排成一条长带。
    ///
    /// **注写在图前也算**（《哈利·波特全集》art.xhtml 的画廊就是
    /// `<p>美版系列封面</p><img>`）：位置不参与判定，只认「一张图 + 最多一段不超字的注」。
    /// 摆的时候由 [`Ctx::children`] 把注挪到图后（这一叠的下面），见 [`Ctx::caption_last`]。
    pub(super) fn image_unit(&self, node: NodeId) -> Option<NodeId> {
        self.image_parts(node).map(|(img, _)| img)
    }

    /// 同 [`Self::image_unit`]，把那段图注也交出来（没有注就是 `None`）。
    pub(super) fn image_parts(&self, node: NodeId) -> Option<(NodeId, Option<NodeId>)> {
        if self.dom.is_element(node, "img") {
            return Some((node, None));
        }
        let mut img: Option<NodeId> = None;
        let mut caption: Option<NodeId> = None;
        for &child in self.dom.get(node).children.iter() {
            let n = self.dom.get(child);
            if n.kind == NodeKind::Text && n.text.trim().is_empty() {
                continue; // 标签之间的换行
            }
            if let Some(found) = single_image(self.dom, child) {
                if img.replace(found).is_some() {
                    return None; // 两张图 → 这是图文混排的一块，不是「图 + 注」
                }
                continue;
            }
            let style = self.style_of(child);
            // 图注那一格：块级、不含图、不含标题、字数在 `STACK_CAPTION_MAX` 以内。
            // 超字数就当成「图后面紧跟的一段正文」，整块不进叠（宁严不宽）。
            //
            // **不要求注已经见过图**：注写在图前是同一件事（哈利波特那本就是这样）。
            let words = text_len_in(self.dom, child);
            if caption.is_some()
                || has_image(self.dom, child)
                || self.is_inline_flow(child, style)
                || self.is_heading(child)
                || words == 0
                || words > STACK_CAPTION_MAX
            {
                return None;
            }
            caption = Some(child);
        }
        img.map(|img| (img, caption))
    }

    /// 这一块的图注写在图**前**时交回那个孩子（叠图那一趟把它挪到队尾用）。
    ///
    /// 「前」按孩子顺序算，不按图的节点位置：图可能裹在一层 `<div>` 里，那一层才是它的
    /// 位置。注本来就在图后的（诡秘那族）回 `None`，那一趟连缓存都照走。
    pub(super) fn caption_before_image(&self, node: NodeId) -> Option<NodeId> {
        let (img, caption) = self.image_parts(node)?;
        let caption = caption?;
        let kids = self.children_of(node);
        let cap_at = kids.iter().position(|kid| *kid == caption)?;
        let img_at = kids
            .iter()
            .position(|kid| single_image(self.dom, *kid) == Some(img))?;
        (cap_at < img_at).then_some(caption)
    }
}
