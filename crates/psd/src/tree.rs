//! Nested layer tree reconstruction from section dividers.
//!
//! In the file, layers are stored bottom-to-top. A group appears as a
//! *bounding section divider* (`lsct` type 3, usually named
//! `</Layer group>`) below its children, followed by the children, followed
//! by the group's own record (`lsct` type 1 = open or 2 = closed folder).

use crate::file::PsdFile;
use crate::tagged::SectionType;

/// A node of the layer tree. Children are in file order (bottom-to-top).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerNode {
    /// A non-group layer.
    Layer {
        /// Index into [`PsdFile::layers`].
        index: usize,
    },
    /// A group (folder).
    Group {
        /// Index of the group's own record (the folder, top of the group).
        index: usize,
        /// Index of the bounding section divider, if one was found.
        divider: Option<usize>,
        /// Children, bottom-to-top.
        children: Vec<LayerNode>,
    },
}

impl LayerNode {
    /// Record index of this node.
    pub fn index(&self) -> usize {
        match self {
            LayerNode::Layer { index } | LayerNode::Group { index, .. } => *index,
        }
    }
    /// Children (empty for layers).
    pub fn children(&self) -> &[LayerNode] {
        match self {
            LayerNode::Layer { .. } => &[],
            LayerNode::Group { children, .. } => children,
        }
    }
    /// `true` for groups.
    pub fn is_group(&self) -> bool {
        matches!(self, LayerNode::Group { .. })
    }
    /// Total number of nodes in this subtree (including self).
    pub fn count(&self) -> usize {
        1 + self.children().iter().map(LayerNode::count).sum::<usize>()
    }
}

impl PsdFile {
    /// Builds the nested layer tree. Root nodes and children are ordered
    /// bottom-to-top (file order).
    ///
    /// Malformed nesting is tolerated: a folder without a matching divider
    /// becomes an empty group; unclosed dividers at the end are dropped and
    /// their children are hoisted to the enclosing level.
    pub fn layer_tree(&self) -> Vec<LayerNode> {
        // Stack of (divider index, children).
        let mut stack: Vec<(Option<usize>, Vec<LayerNode>)> = vec![(None, Vec::new())];
        for (i, rec) in self.layers().iter().enumerate() {
            match rec.section_type() {
                SectionType::BoundingDivider => stack.push((Some(i), Vec::new())),
                SectionType::OpenFolder | SectionType::ClosedFolder => {
                    let (divider, children) = if stack.len() > 1 { stack.pop().unwrap_or_default() } else { (None, Vec::new()) };
                    if let Some(top) = stack.last_mut() {
                        top.1.push(LayerNode::Group { index: i, divider, children });
                    }
                }
                _ => {
                    if let Some(top) = stack.last_mut() {
                        top.1.push(LayerNode::Layer { index: i });
                    }
                }
            }
        }
        while stack.len() > 1 {
            let (_, children) = stack.pop().unwrap_or_default();
            if let Some(top) = stack.last_mut() {
                top.1.extend(children);
            }
        }
        stack.pop().map(|(_, c)| c).unwrap_or_default()
    }
}
