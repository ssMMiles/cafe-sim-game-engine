# Cafe Sim Development Memory

## Recipe Card Interaction Fix (2025-01-20)

Fixed recipe card hover/interaction issues that were especially problematic at close distances.

### Problem
- Recipe cards were not reliably interactable, especially from up close
- Cards would sometimes work from certain angles/distances but not others
- Ray casting was inconsistent for these UI elements

### Root Causes
1. **Using Transform instead of GlobalTransform**: The interaction system was using local transforms for distance calculations on child entities
2. **Insufficient Z offset**: Cards were only 0.033 units (0.1/3.0) away from parent menu, causing ray occlusion
3. **Parent plane interference**: The parent menu's plane could block rays from hitting the cards

### Solution
1. **Updated interaction system to use GlobalTransform**:
   - Changed `check_interactions` query from `&Transform` to `&GlobalTransform`
   - Use `global_transform.translation()` for world-space position calculations
   - This fixes all child entity interactions, not just recipe cards
2. **Added GlobalTransform::default()** to all recipe card entities
3. **Increased Z offset** from 0.1/3.0 (≈0.033) to 0.2 units for better separation
4. **Updated hover animation** to move cards from 0.2 to 0.3 when hovered (was 0.033 to 0.1)
5. **Adjusted shadow positions** to Z=0.05 to stay between menu (Z=0) and cards (Z=0.2+)
6. **Updated text overlays** to Z=0.21 to stay just in front of cards

### Benefits
- Recipe cards (and all child entities) now use correct world positions for interaction
- Better visual depth separation between UI layers
- More consistent ray-mesh intersection for small UI elements in 3D space
- Fixes the fundamental issue of child entity transforms in the interaction system

## Extended Interaction System (2025-01-19)

### Overview
Refactored the interaction system to be more generic and extensible, making it easy to add new interactive objects.

### Changes Made

1. **Renamed BeingInteractedWith to HoveredOver**
   - Better represents that the entity is being looked at/hovered over
   - Updated across `interaction.rs`, `simulation.rs`, and `ui_displays.rs`

2. **Moved InteractableType into Interactable struct**
   - Moved enum from `simulation.rs` to `interaction.rs`
   - Added `interactable_type` field to `Interactable` struct
   - No longer a separate component, simplifying queries

3. **Added Event-Based Click System**
   - Created `InteractableClicked` event containing entity and full `Interactable` data
   - Added `detect_interactable_clicks` system that emits events when clicking on `HoveredOver` entities
   - Created unified `handle_interactable_clicks` function that dispatches to type-specific handlers

4. **Refactored Cutting Board Handler**
   - Changed from directly checking mouse input to using event system
   - Extracted logic into `handle_cutting_board_click` helper function
   - Now called from the unified click handler based on `InteractableType`

### Benefits
- **Extensible**: Add new interactable types by adding enum variant and handler case
- **Event-driven**: Multiple systems can respond to the same click event
- **Cleaner**: InteractableType is part of core Interactable data
- **Unified**: Single entry point for all interaction handling

### Usage Example
To add a new interactable type:
1. Add variant to `InteractableType` enum
2. Create objects with `Interactable { interactable_type: NewType, ... }`
3. Add case in `handle_interactable_clicks` match statement

## Material System Refactor (2025-01-19)
- Created new `materials.rs` module in `src/game/` to handle material-related functions
- Moved `update_mesh_materials_color` and `create_arc_mesh` functions from `simulation.rs` to `materials.rs`
- Added `apply_pending_materials` system to handle GLTF scene material updates
- Updated imports in `simulation.rs` to use functions from the materials module
- Added the materials module to `mod.rs` and integrated `apply_pending_materials` into the plugin's update systems

## Cooking Display System (Added 2025-06-14)

Implemented a system to display cooking progress on tables/cutting boards:

1. **Components:**
   - `CookingDisplay`: Component that tracks which table entity a display belongs to
   - `ActiveCooking`: Already existed, tracks recipe name and timing on tables

2. **Systems:**
   - `display_cooking_progress`: Creates visual indicators for tables with ActiveCooking but no display
   - `update_cooking_displays`: Updates progress bars based on remaining cooking time
   - Modified `update_cooking_timers`: Now finds and removes associated CookingDisplay entities when cooking completes

3. **Visual Design:**
   - 3D display positioned 2.0 units above table surface in world space
   - Always faces the camera (billboard-style)
   - Beige background card (1.0x0.8) matching cutting board menu color
   - Components layout:
     - Grey circle outline (background ring, radius 0.185) at y: 0.1
     - Green progress arc (fills clockwise as cooking progresses, radius 0.175) at y: 0.1
     - Recipe icon below (0.3x0.3) at y: -0.25
   - All child elements have proper Visibility::Inherited to fix hierarchy warnings

4. **Key Implementation Details:**
   - Uses world positioning instead of parent-child hierarchy (fixes scaling issue)
   - CookingDisplay stores reference to its table entity
   - Uses Z-normal planes to face forward instead of Y-normal (upward)
   - Custom arc mesh generation for circular progress indicator
   - Both systems run in the main Update schedule when in Running simulation state

5. **Additional Components:**
   - `ProgressArc`: Marker component for circular progress indicators

6. **New Systems:**
   - `update_cooking_display_rotation`: Keeps displays facing the camera
   - `create_arc_mesh`: Helper function to generate circular progress arcs based on progress

7. **Menu Behavior:**
   - Menu automatically closes when a recipe is selected and cooking starts
   - Menu cannot be opened while cutting board is cooking
   - Menu only opens when cutting board is idle

8. **Fixed Issues:**
   - Display orientation: Changed from facing sky to facing camera
   - Progress indicator: Changed from linear bar to circular progress ring
   - Menu interaction: Added auto-close and cooking state checks
   - Positioning: Fixed scaling issues with world space positioning

9. **Dynamic Background System (Added 2025-06-14):**
   - **DynamicBackground Component**: Added component with padding (0.15) and min_size (0.8x0.6) fields
   - **update_dynamic_backgrounds System**: Calculates optimal background size based on child element bounds
     - Traverses children to find content elements (progress circle, recipe icon)
     - Calculates bounding box from Transform positions and known element sizes
     - Progress circle: radius 0.185, Recipe icon: 0.3x0.3
     - Adds padding around content and ensures minimum size requirements
     - Generates new mesh with calculated size and updates Mesh3d component
   - **Modified display_cooking_progress**: Now spawns displays with DynamicBackground component
   - **System Order**: Runs after display_cooking_progress but before update_cooking_display_rotation
   - **Benefits**: Background automatically adapts to different layouts, consistent padding, reusable for other UI

## Previous Notes

- The game uses a state-driven architecture with AppState and SimulationState
- Mesh raycasting is used for precise object interaction
- SVG icons are rendered to textures for use in 3D UI elements