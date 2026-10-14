//! MOBI / AZW / AZW3 这一族的**格式知识**（三条地基已落地，装配与 provider 还没接）。
//!
//! 容器字节不在这里：PalmDB 的记录寻址在 [`super::palmdb`]，这一族只管「记录里的字节
//! 是什么意思」。分家的理由是 `plans/导入管线.md` 第十节那一条 —— 容器家族比格式少，
//! 寻址只该写一份。
//!
//! ## 已经有了的三块
//!
//! | 模块 | 干什么 | 谁要用 |
//! | --- | --- | --- |
//! | [`codec`] | PalmDOC 与 HUFF/CDIC 两种解压，加上记录尾部 `extra_flags` 那段的剥除 | KF7 与 KF8 的正文记录都是这两种码 |
//! | [`indx`] | INDX / TAGX / vwi / CNCX 一套索引机制 | NCX 目录读它，KF8 的 SKEL 与 FRAG 也读它 |
//! | [`encoding`] | 头里 `text encoding`（记录 0 偏移 28）那三档解码 | 两族共用 |
//!
//! ## 还缺的那一步（以及它为什么不能省）
//!
//! `MobiAdapter`（provider）与「读 KF8 分区还是 KF7 兼容半边」那条分岔。三件事决定了
//! 这一步不是把 [`super::epub`] 那套改改就能用：
//!
//! **一、AZW3 的内容没有条目名。** KF8 是一整条 `flow` 字节区，「一个文件」是按 SKEL
//! 与 FRAG 两条 INDX 拼出来的区间（`libmobi` 的 `parse_rawml.c`），所以
//! [`super::ChapterLocator::Entry`] 那条「按名字回容器现取」的位置形状在这族上走不通，
//! 得另定一档 —— 那是 [`super`] 层的 `Address`/locator 第一次长出不属于名单式容器的分支。
//!
//! **二、hybrid 文件里两半各有自己的头。** EXTH 121 那个值是**要减一**再验 `BOUNDARY`
//! 魔数的，KF8 分区是**第二个**记录 0，`kf8offset = boundary + 1`；索引字段里
//! skel/frag/guide/ncx/orth/infl/fdst/huff 与首条正文记录都要加这个偏移，
//! 而 `image_index` **不加**（hybrid 文件的图片用的是 KF7 头里那一格）。
//!
//! **三、正文记录每帧 4096 字节、独立解压、解出的长度还不一样。** 所以
//! `filepos / 4096` 这种直接除法只能定位到「第几帧」，定位不到字节：要先攒一张
//! 各帧解出长度的累计表，再用帧内余数去加（见 [`super::palmdb`] 模块头第三条：
//! PalmOS 的尾部填充也在这里被剥掉）。
//!
//! 参考实现有两份，都在 `reference/` 下、都不在构建里：`libmobi`（唯一真读 KF8 的，
//! LGPL，只读不抄）与 crengine 的 `pdbfmt.cpp`（PalmDOC/EXTH 字段偏移的好证人，
//! 但它明确拒绝 KF8，也没实现 HUFF/CDIC）。

#[allow(dead_code)] // 装配那一步（KF8/KF7 分岔与 provider）由下一轮接上。
mod codec;
#[allow(dead_code)]
mod encoding;
#[allow(dead_code)]
mod indx;

#[allow(dead_code)]
pub(crate) use codec::{HuffCdic, extras_len, palmdoc_decompress};
#[allow(dead_code)]
pub(crate) use encoding::{decode_text, is_supported as codepage_supported};
#[allow(dead_code)]
pub(crate) use indx::{TocItem, read_ncx};
