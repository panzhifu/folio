//! 「网络导入」那张卡：把已有的 Wi-Fi 传书服务摆到读者眼前。
//!
//! 服务本体在 [`folio_core::net::transfer::TransferServer`]，启停编排在
//! [`crate::ui::services::transfer`]，设置页里也有一份开关。这一层只做三件事：
//! 打开卡片时确保服务起着、把服务报的第一个地址画成二维码、把地址摆出来。
//!
//! **二维码是给别的设备扫的**：内容就是那个 `http://<设备IP>:<端口>/`，扫出来直接是
//! 传书网页（拖文件即上传），省得在浏览器里手输一长串地址。服务不设配对码，所以
//! 扫开就能用；这里只生成，不需要相机权限，也不碰扫码解码那一条路。
//!
//! 容器与扫描那一屏**同一套**：桌面是 [`overlay::centered`] 的居中浮层，手机是
//! [`overlay::sheet`] 的底部弹层（项目自写的 `BottomSheet`，不是框架的 `Sheet`）。
//!
//! 打开卡片就起服务、关掉卡片**不停**服务：读者很可能要切到另一台设备上去传书，
//! 卡片一收就把服务掐掉等于让他白扫一次。停服务是卡上那颗显式按钮，与设置页那个
//! 开关同一条「服务不写进配置、重启不自动开」的立场。

use std::io::Cursor;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::separator::Separator;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, ClipboardItem, Context, FontWeight, Image, ImageFormat, InteractiveElement,
    IntoElement, ObjectFit, ParentElement, SharedString, Styled, StyledImage as _,
    TestSupportExt as _, div, img, px,
};
use image::Luma;
use qrcode::QrCode;

use crate::ui::components::muted_label;
use crate::ui::services::l10n::tr;
use crate::ui::services::transfer;

use super::super::overlay;
use super::ShelfView;
use super::grid::GUTTER;

/// 二维码的边长（逻辑像素）。地址串约 30 字节，QR 版本落在 3 那一档，
/// `min_dimensions` 会把模块放大到这个尺寸 —— 手机上按得住、也扫得出来。
const QR_EDGE: f32 = 200.0;
const QR_MIN_PX: u32 = 200;

/// 一个网址 → 一张黑白二维码的 PNG 字节。
///
/// 走 `qrcode` 的 `image` feature（与锁里那份 image 0.25 同一个 crate），再借 workspace
/// 已开的 png 编码写成字节，交给 gpui 的 [`Image::from_bytes`]。**不带纠错等级参数**：
/// 默认 M 档对 30 字节的 URL 绰绰有余，静区（quiet zone）必须留，不然多数扫码器认不出。
/// 编码失败只回 `None`：卡片退化成「只有地址可复制」，不该让一次编码把整张卡拖没。
fn qr_png(url: &str) -> Option<Vec<u8>> {
    let code = QrCode::new(url).ok()?;
    let mut renderer = code.render::<Luma<u8>>();
    renderer
        .quiet_zone(true)
        .min_dimensions(QR_MIN_PX, QR_MIN_PX);
    let bitmap = renderer.build();
    let mut png = Vec::new();
    image::DynamicImage::ImageLuma8(bitmap)
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

impl ShelfView {
    /// 卡片内容：标题、说明、方框地址（右侧一颗复制图标）、二维码。两端共用，
    /// 宽度由容器给。
    ///
    /// **没有按钮行**：收卡靠点外面 / 返回键（桌面浮层与手机弹层各自的路），而收卡
    /// 就是停服务（[`Self::close_network_import`]）—— 服务不设口令，开着等于把局域
    /// 网里的入口一直敞着，读者关掉这张卡就是「我用完了」。
    ///
    /// 与建组那张表单同一条：这里不带横向留白、也不带 `flex_1` —— 桌面浮层是定宽的，
    /// 手机弹层天生满幅，写在这里两头都会多算一层。
    fn network_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let urls = transfer::running()
            .map(|status| status.urls)
            .unwrap_or_default();
        let copy_label = tr(cx, "shelf.network_copy");

        let mut card = v_flex()
            .id("shelf-network-import")
            .test_support()
            .w_full()
            .gap_3()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "shelf.network_import")),
            )
            .child(muted_label(tr(cx, "shelf.network_hint"), cx))
            .child(Separator::horizontal());

        if urls.is_empty() {
            // 服务起着却没探到局域网地址（多网卡 / 飞行模式那一类）：只摆提示，
            // 不摆一个编着空串的二维码 —— 扫出来什么都没有比没有更糟。
            card = card.child(muted_label(tr(cx, "shelf.network_no_url"), cx));
        } else {
            for (ix, url) in urls.iter().enumerate() {
                let copy = url.clone();
                let label = copy_label.clone();
                // 地址用方框框住：读者一眼看出「这一串是要拿去用的值」，而不是一句
                // 正文；复制图标贴着右边，与那串字的右端对齐。
                card = card.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(cx.theme().muted)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(url.clone()),
                        )
                        .child(
                            Button::new(SharedString::from(format!("shelf-network-copy-{ix}")))
                                .ghost()
                                .small()
                                .icon(IconName::Copy)
                                .tooltip(label.clone().to_string())
                                .accessibility_label(label)
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                }),
                        ),
                );
            }
            if let Some(qr) = &self.network_qr {
                card = card.child(
                    div().w_full().flex().items_center().justify_center().child(
                        img(qr.clone())
                            .w(px(QR_EDGE))
                            .h(px(QR_EDGE))
                            .object_fit(ObjectFit::Contain),
                    ),
                );
            }
        }

        card.into_any_element()
    }

    /// 桌面那一档：屏幕居中的一张卡，与扫描、建组那几张同一个容器与同一个宽度。
    pub(in crate::ui::features::shelf) fn network_popover(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shelf = cx.entity().clone();
        let urls = transfer::running()
            .map(|status| status.urls.len())
            .unwrap_or(0);
        let body = div()
            .w(px(overlay::FORM_WIDTH))
            .child(self.network_body(cx))
            .into_any_element();
        overlay::centered(
            "shelf-network-popover",
            network_height(&self.network_qr, urls),
            &shelf,
            |this, cx| this.close_network_import(cx),
            body,
        )
    }

    /// 手机那一档：底部弹层，与扫描那一屏同一个 `BottomSheet`。
    ///
    /// 内容没收稳就不给 —— 与 `scan_sheet` 同一条（退场那半程还浮在屏上，断掉内容
    /// 等于看着它空着沉下去）。卡片里没有输入框，键盘那一抬用不上。
    pub(in crate::ui::features::shelf) fn network_sheet(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body = (!self.network_sheet.read(cx).is_idle()).then(|| {
            v_flex()
                .w_full()
                .px(px(GUTTER))
                .pb_4()
                .child(self.network_body(cx))
                .into_any_element()
        });
        overlay::sheet(
            "shelf-network-sheet",
            &self.network_sheet,
            overlay::SHEET_EXPANDED,
            false,
            body,
        )
    }
}

/// 整张卡的估高，供 `overlay::centered` 往上抬掉半张。二维码那一档多 200 像素，
/// 没二维码（没探到地址）少一截 —— 报的数与画出来的差几像素只让卡偏一点，不截断。
fn network_height(qr: &Option<Arc<Image>>, urls: usize) -> f32 {
    let qr = if qr.is_some() { QR_EDGE } else { 0.0 };
    // 一个地址框约 36 高；一个都没有时给提示那一行留 20。
    let addr = if urls == 0 { 20.0 } else { urls as f32 * 36.0 };
    // 标题 + 说明 + 分隔线 + 地址框 + 二维码。
    52.0 + 40.0 + 4.0 + addr + qr
}

/// 把一张二维码 PNG 解成 gpui 的图片。抽成函数是为了让 [`ShelfView::open_network_import`]
/// 那边只管「开了服务、拿到地址」，解码失败时状态里就留 `None`。
pub(in crate::ui::features::shelf) fn qr_image(url: &str) -> Option<Arc<Image>> {
    let png = qr_png(url)?;
    Some(Arc::new(Image::from_bytes(ImageFormat::Png, png)))
}

#[cfg(test)]
mod tests {
    use super::qr_png;

    /// 编码路径不能悄悄退化：卡片上没二维码＝读者没法扫，而这类失败在界面上
    /// 只表现为「少了一块」，没有报错。钉住一条真实形状的地址能产出 PNG。
    #[test]
    fn a_transfer_url_encodes_to_a_png() {
        let png = qr_png("http://192.168.1.5:8410/").expect("地址要编得出二维码");
        assert!(
            png.starts_with(&[0x89, b'P', b'N', b'G']),
            "交给 Image::from_bytes 的得是 PNG"
        );
        assert!(png.len() > 100, "不是一张空图");
    }
}
