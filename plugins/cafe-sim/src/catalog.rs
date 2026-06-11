use bevy::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PieceKind {
    Decoration,
    Countertop,
    CoffeeMachine,
    GarbageCan,
}

#[derive(Clone, Copy)]
pub struct PieceDef {
    pub name: &'static str,
    pub asset_path: &'static str,
    pub kind: PieceKind,
    /// Linear-ish sRGB tint applied to the piece's mesh material in both the
    /// placement preview and the placed object. Stored as a plain array so the
    /// catalog can live in a `const`. Only used when [`PieceDef::tint`] is true.
    pub color: [f32; 3],
    /// When true, every mesh of the piece is repainted with [`PieceDef::color`]
    /// (the placeholder look). Set false to keep the model's own material — e.g.
    /// a real baked asset that already looks right.
    pub tint: bool,
}

impl PieceDef {
    pub fn color(&self) -> Color {
        Color::srgb(self.color[0], self.color[1], self.color[2])
    }
}

#[derive(Clone, Copy)]
pub struct CategoryDef {
    pub name: &'static str,
    pub pieces: &'static [PieceDef],
}

/// Look up a piece in the static catalog by stable indices. Returns `None`
/// if either index is out of range — used by the save-loader to skip
/// records that reference pieces no longer present in the catalog.
pub fn lookup(category_idx: u8, piece_idx: u8) -> Option<&'static PieceDef> {
    let cat = CATALOG.get(category_idx as usize)?;
    cat.pieces.get(piece_idx as usize)
}

pub const CATALOG: &[CategoryDef] = &[
    CategoryDef {
        name: "Surfaces",
        pieces: &[PieceDef {
            name: "Table",
            asset_path: "cafe-sim/tabletop-baked/Mesh_0.gltf#Scene0",
            kind: PieceKind::Decoration,
            color: [0.25, 0.45, 0.85],
            tint: false,
        }],
    },
    CategoryDef {
        name: "Equipment",
        pieces: &[
            PieceDef {
                name: "Countertop",
                asset_path: "cafe-sim/Tabletop.glb#Scene0",
                kind: PieceKind::Countertop,
                color: [0.30, 0.70, 0.35],
                tint: true,
            },
            PieceDef {
                name: "Coffee Machine",
                asset_path: "cafe-sim/Tabletop.glb#Scene0",
                kind: PieceKind::CoffeeMachine,
                color: [0.80, 0.25, 0.25],
                tint: true,
            },
            PieceDef {
                name: "Garbage Can",
                asset_path: "cafe-sim/Tabletop.glb#Scene0",
                kind: PieceKind::GarbageCan,
                color: [0.08, 0.08, 0.10],
                tint: true,
            },
        ],
    },
];
