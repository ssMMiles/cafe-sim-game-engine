use bevy::prelude::*;

use crate::grid::Turn;

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
    /// Cells the piece covers, along the shell's X then Z, before the player
    /// turns it. The model is authored to fill that many cells at
    /// [`crate::grid::PIECE_SCALE`], centred on the footprint.
    pub footprint: (u8, u8),
    /// How the model stands before the player turns it, so a piece authored
    /// facing +Z can still rest facing into the room (away from the door).
    /// Composed with the player's turn; saves store only the player's.
    pub facing: Turn,
}

impl PieceDef {
    pub fn footprint(&self) -> (i32, i32) {
        (self.footprint.0 as i32, self.footprint.1 as i32)
    }

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
            footprint: (1, 1),
            facing: Turn::None,
        }],
    },
    CategoryDef {
        name: "Equipment",
        pieces: &[
            PieceDef {
                name: "Countertop",
                asset_path: "cafe-sim/Counter.gltf#Scene0",
                kind: PieceKind::Countertop,
                color: [0.30, 0.70, 0.35],
                tint: false,
                // A setpiece: three cells long. The placeholder mesh is one
                // cell wide and sits on the middle one until the real
                // counter lands.
                footprint: (3, 1),
                // Authored facing the door; serve from the room side.
                facing: Turn::Half,
            },
            PieceDef {
                name: "Coffee Machine",
                asset_path: "cafe-sim/Tabletop.glb#Scene0",
                kind: PieceKind::CoffeeMachine,
                color: [0.80, 0.25, 0.25],
                tint: true,
                footprint: (1, 1),
                facing: Turn::None,
            },
            PieceDef {
                name: "Garbage Can",
                asset_path: "cafe-sim/Tabletop.glb#Scene0",
                kind: PieceKind::GarbageCan,
                color: [0.08, 0.08, 0.10],
                tint: true,
                footprint: (1, 1),
                facing: Turn::None,
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_piece_covers_at_least_one_cell() {
        for cat in CATALOG {
            for piece in cat.pieces {
                assert!(
                    piece.footprint.0 >= 1 && piece.footprint.1 >= 1,
                    "{} has an empty footprint",
                    piece.name
                );
            }
        }
    }
}
