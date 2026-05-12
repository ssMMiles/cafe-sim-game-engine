use bevy::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PieceKind {
    Decoration,
    Countertop,
    CoffeeMachine,
}

#[derive(Clone, Copy)]
pub struct PieceDef {
    pub name: &'static str,
    pub asset_path: &'static str,
    pub kind: PieceKind,
    /// Linear-ish sRGB tint applied to the piece's mesh material in both the
    /// placement preview and the placed object. Stored as a plain array so the
    /// catalog can live in a `const`.
    pub color: [f32; 3],
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

pub const CATALOG: &[CategoryDef] = &[
    CategoryDef {
        name: "Surfaces",
        pieces: &[PieceDef {
            name: "Table",
            asset_path: "cafe-sim/Tabletop.glb#Scene0",
            kind: PieceKind::Decoration,
            color: [0.25, 0.45, 0.85],
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
            },
            PieceDef {
                name: "Coffee Machine",
                asset_path: "cafe-sim/Tabletop.glb#Scene0",
                kind: PieceKind::CoffeeMachine,
                color: [0.80, 0.25, 0.25],
            },
        ],
    },
];
