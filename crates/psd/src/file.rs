//! The top-level [`PsdFile`] model, parser and writer.

use crate::error::{PsdError, Result};
use crate::header::{ColorMode, Header, Version};
use crate::image_data::ImageData;
use crate::io::{Reader, WriteExt};
use crate::layer::{LayerInfo, LayerRecord};
use crate::resources::{self, ImageResource, ResourceData, ids};
use crate::tagged::{TaggedBlock, read_blocks, write_blocks};

/// Where the layer info was stored in the file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum LayerInfoPlacement {
    /// In the layer info field of the layer and mask section (8-bit files).
    #[default]
    Section,
    /// In a global `Lr16` / `Lr32` / `Layr` tagged block (16/32-bit files).
    /// The block is re-inserted at `index` among `global_blocks` on write.
    GlobalBlock {
        /// Position among the global blocks.
        index: usize,
        /// Block signature (`8BIM` / `8B64`).
        signature: [u8; 4],
        /// Block key.
        key: [u8; 4],
        /// Padding after the block (see [`TaggedBlock::padding`]).
        padding: Option<Vec<u8>>,
    },
}

/// Global layer mask info, stored raw with typed accessors.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GlobalLayerMask {
    /// Raw bytes (without the length field).
    pub data: Vec<u8>,
}

impl GlobalLayerMask {
    fn be16(&self, at: usize) -> Option<u16> {
        self.data.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    /// Overlay color space.
    pub fn overlay_color_space(&self) -> Option<u16> {
        self.be16(0)
    }
    /// Four color components.
    pub fn color_components(&self) -> Option<[u16; 4]> {
        Some([self.be16(2)?, self.be16(4)?, self.be16(6)?, self.be16(8)?])
    }
    /// Opacity 0..=100.
    pub fn opacity(&self) -> Option<u16> {
        self.be16(10)
    }
    /// Kind: 0 color selected, 1 color protected, 128 per layer.
    pub fn kind(&self) -> Option<u8> {
        self.data.get(12).copied()
    }
}

/// A complete PSD/PSB file.
///
/// `PsdFile::from_bytes(b)?.to_bytes()? == b` for unmodified well-formed
/// files, and `from_bytes(to_bytes(f)) == f` for any model `f`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PsdFile {
    /// Header.
    pub header: Header,
    /// Color mode data (palette for indexed, duotone spec, …).
    pub color_mode_data: Vec<u8>,
    /// Image resources in stored order.
    pub resources: Vec<ImageResource>,
    /// Layer info (`None` when the layer info length is 0 and no
    /// `Lr16`/`Lr32` block holds layers).
    pub layer_info: Option<LayerInfo>,
    /// Where [`Self::layer_info`] is stored.
    pub layer_info_placement: LayerInfoPlacement,
    /// Global layer mask (`None` when the field is absent entirely).
    pub global_layer_mask: Option<GlobalLayerMask>,
    /// Global additional layer information blocks.
    pub global_blocks: Vec<TaggedBlock>,
    /// Bytes at the end of the layer and mask section that do not form a block.
    pub layer_mask_trailing: Vec<u8>,
    /// Merged image data.
    pub image_data: ImageData,
}

impl PsdFile {
    /// Parses a PSD or PSB file.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let header = Header::read(&mut r)?;
        let psb = header.version.is_psb();
        let cmd_len = r.u32()?;
        let color_mode_data = r.bytes_u64(u64::from(cmd_len))?.to_vec();
        let resources = resources::read_section(&mut r)?;

        let lm_len = r.len_field(psb)?;
        let mut s = r.sub(lm_len)?;
        let mut layer_info = None;
        let mut global_layer_mask = None;
        let mut global_blocks = Vec::new();
        let mut layer_mask_trailing = Vec::new();
        let mut layer_info_placement = LayerInfoPlacement::Section;
        if !s.is_empty() {
            let li_len = s.len_field(psb)?;
            if li_len > 0 {
                layer_info = Some(LayerInfo::read_body(s.bytes_u64(li_len)?, header.version)?);
            }
            if s.remaining() >= 4 {
                let n = s.u32()?;
                global_layer_mask = Some(GlobalLayerMask { data: s.bytes_u64(u64::from(n))?.to_vec() });
                let (blocks, trailing) = read_blocks(&mut s, header.version)?;
                global_blocks = blocks;
                layer_mask_trailing = trailing;
            } else {
                layer_mask_trailing = s.peek_rest().to_vec();
            }
        }
        // 16/32-bit files keep their layers in a global Lr16/Lr32 block.
        if layer_info.is_none()
            && let Some(i) = global_blocks.iter().position(|b| matches!(&b.key, b"Lr16" | b"Lr32" | b"Layr"))
            && let Ok(info) = LayerInfo::read_body(&global_blocks[i].data, header.version)
        {
            let b = global_blocks.remove(i);
            layer_info = Some(info);
            layer_info_placement = LayerInfoPlacement::GlobalBlock { index: i, signature: b.signature, key: b.key, padding: b.padding };
        }
        let image_data = ImageData::read(&mut r, &header)?;
        Ok(PsdFile { header, color_mode_data, resources, layer_info, layer_info_placement, global_layer_mask, global_blocks, layer_mask_trailing, image_data })
    }

    /// Serializes the file.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let v = self.header.version;
        let psb = v.is_psb();
        let mut out = Vec::new();
        self.header.write(&mut out);
        out.put_len(self.color_mode_data.len() as u64, false)?;
        out.put(&self.color_mode_data);
        resources::write_section(&mut out, &self.resources)?;

        let in_block = matches!(self.layer_info_placement, LayerInfoPlacement::GlobalBlock { .. });
        // An entirely empty section is written as a zero length. (A section
        // containing only a zero layer-info length cannot be distinguished in
        // the model and is normalized to this form.)
        let section_empty =
            self.layer_info.is_none() && self.global_layer_mask.is_none() && self.global_blocks.is_empty() && self.layer_mask_trailing.is_empty();
        let at = out.begin_len(psb);
        if !section_empty {
            let li = out.begin_len(psb);
            if let (Some(info), false) = (&self.layer_info, in_block) {
                info.write_body(&mut out, v)?;
            }
            out.end_len(li, psb)?;
            let blocks: std::borrow::Cow<'_, [TaggedBlock]> = match (&self.layer_info, &self.layer_info_placement) {
                (Some(info), LayerInfoPlacement::GlobalBlock { index, signature, key, padding }) => {
                    let mut data = Vec::new();
                    info.write_body(&mut data, v)?;
                    let mut blocks = self.global_blocks.clone();
                    let idx = (*index).min(blocks.len());
                    blocks.insert(idx, TaggedBlock { signature: *signature, key: *key, data, padding: padding.clone() });
                    blocks.into()
                }
                _ => (&self.global_blocks[..]).into(),
            };
            let blocks_ref: &[TaggedBlock] = &blocks;
            let need_mask_field = self.global_layer_mask.is_some() || !blocks_ref.is_empty();
            if need_mask_field {
                let gm = self.global_layer_mask.as_ref().map_or(&[][..], |g| &g.data[..]);
                out.put_len(gm.len() as u64, false)?;
                out.put(gm);
            }
            write_blocks(&mut out, blocks_ref, v)?;
            out.put(&self.layer_mask_trailing);
        }
        out.end_len(at, psb)?;

        out.put_u16(self.image_data.compression.as_u16());
        out.put(&self.image_data.data);
        Ok(out)
    }

    /// Layer records in file order (bottom-most first); empty if none.
    pub fn layers(&self) -> &[LayerRecord] {
        self.layer_info.as_ref().map_or(&[], |i| &i.layers)
    }

    /// Mutable layer records (creates an empty layer info if absent).
    pub fn layers_mut(&mut self) -> &mut Vec<LayerRecord> {
        &mut self.layer_info.get_or_insert_with(LayerInfo::default).layers
    }

    /// First resource with the given id.
    pub fn resource(&self, id: u16) -> Option<&ImageResource> {
        self.resources.iter().find(|r| r.id == id)
    }

    /// First global block with the given key.
    pub fn global_block(&self, key: &[u8; 4]) -> Option<&TaggedBlock> {
        self.global_blocks.iter().find(|b| &b.key == key)
    }

    /// ICC profile bytes (resource 1039).
    pub fn icc_profile(&self) -> Option<&[u8]> {
        self.resource(ids::ICC_PROFILE).map(|r| &r.data[..])
    }

    /// Resolution info (resource 1005).
    pub fn resolution(&self) -> Option<crate::resources::ResolutionInfo> {
        match self.resource(ids::RESOLUTION_INFO)?.parsed()? {
            Ok(ResourceData::ResolutionInfo(r)) => Some(r),
            _ => None,
        }
    }

    /// The "has real merged data" flag of resource 1057, if present.
    pub fn has_real_merged_data(&self) -> Option<bool> {
        match self.resource(ids::VERSION_INFO)?.parsed()? {
            Ok(ResourceData::VersionInfo(v)) => Some(v.has_real_merged_data),
            _ => None,
        }
    }

    /// Decodes the merged image into planar big-endian samples.
    pub fn decode_merged(&self) -> Result<Vec<u8>> {
        self.image_data.decode(&self.header)
    }

    /// `true` if the merged image carries a transparency channel.
    pub fn merged_has_alpha(&self) -> bool {
        let Some(color) = self.header.color_mode.color_channels() else {
            return false;
        };
        if self.header.channels <= color {
            return false;
        }
        match &self.layer_info {
            Some(li) => li.merged_alpha,
            None => true,
        }
    }

    /// Checks structural invariants that the writer relies on.
    pub fn validate(&self) -> Result<()> {
        self.header.validate()?;
        if self.header.version == Version::Psd && (self.header.width > 30_000 || self.header.height > 30_000) {
            return Err(PsdError::LimitExceeded("PSD dimensions exceed 30000; use PSB"));
        }
        if self.header.color_mode == ColorMode::Indexed && self.color_mode_data.len() != 768 {
            return Err(PsdError::invalid("indexed color mode requires a 768-byte palette"));
        }
        Ok(())
    }

    /// Reads a file from disk (native targets only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(path: impl AsRef<std::path::Path>) -> std::io::Result<Result<Self>> {
        Ok(Self::from_bytes(&std::fs::read(path)?))
    }

    /// Writes the file to disk (native targets only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        let b = self.to_bytes().map_err(std::io::Error::other)?;
        std::fs::write(path, b)
    }
}
