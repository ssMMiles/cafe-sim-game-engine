#[derive(Clone, Copy)]
pub struct PieceDef {
    pub name: &'static str,
    pub asset_path: &'static str,
}

#[derive(Clone, Copy)]
pub struct CategoryDef {
    pub name: &'static str,
    pub pieces: &'static [PieceDef],
}

pub const CATALOG: &[CategoryDef] = &[CategoryDef {
    name: "Surfaces",
    pieces: &[PieceDef {
        name: "Tabletop",
        asset_path: "cafe-sim/Tabletop.glb#Scene0",
    }],
}];
