# Memory

## Object Placement System Update - January 2025

Added a feature to cycle through different objects to place in edit mode:

- **New PlacementObjectType enum**: Added to settings.rs with variants: Table, Cooker, Fridge, CuttingBoard
- **Settings Integration**: Added current_placement_object field to UserSettings and cycle_placement_object() method
- **Keybind**: Shift+R cycles through placement objects, displayed in the keybinds UI
- **Object Types**: Currently all use the same table mesh but with different colors:
  - Table: Brown wood (0.6, 0.4, 0.2)
  - Cooker: Bright red (0.9, 0.2, 0.2) with slight metallic finish
  - Fridge: Blue (0.2, 0.4, 0.8) with metallic finish
  - Cutting Board: Green (0.2, 0.8, 0.3)
- **Marker Components**: Added CookerMarker, FridgeMarker, CuttingBoardMarker components for identifying object types
- **Material System**: Updated override_table_materials to override_object_materials to handle all object types

The system is designed to be easily extended - when proper 3D models are available for each object type, only the asset loading in handle_edit_mode_click needs to be updated.

## Green Transparent Placement Preview - January 2025

Added a visual preview system that shows where objects will be placed in edit mode:

- **PlacementPreview Component**: Marks the preview entity
- **PlacementPreviewEntity Resource**: Tracks the current preview entity 
- **Preview Systems**:
  - `setup_placement_preview`: Creates preview entity when entering edit mode
  - `cleanup_placement_preview`: Removes preview when exiting edit mode  
  - `update_placement_preview`: Updates preview position to follow mouse cursor
  - `apply_preview_material`: Applies green transparent material to preview meshes
- **Visual Features**:
  - Green semi-transparent material (50% alpha) for clear preview indication
  - Slightly elevated (+0.05 units) to prevent z-fighting with ground
  - Hidden when no valid placement position available
  - Uses same mesh as actual objects for accurate preview
- **Integration**: Preview position is used by `handle_edit_mode_click` for precise placement alignment