# Engine

Large Features:
 - Terrain painting tool (support multiple textures, blending, and applying it automatically based on height rules etc)
 - Modeling tool for basic meshes (blocks, walls) to quickly build out rooms/levels
 - Separate character controller in editor from ingame, require game plugin to offer their own

Fixes/Reviews:
 - Overview of edge detection shader, edges sometimes missing especially when looking down from above or at certain angles. Would like access to the variable properties for the shader in the editor.
  
Misc:
 - Editor toggle to view collision boxes
 - Editor toggle to hide gizmos on lights
 - Make "world: x" display into a clickable dropdown rather than hiding world switch behind menus
 - Better naming scheme for newly placed items (Rather than call it mesh, use Name + number on repeat placements if first hasnt been renamed)

# Cafe Sim
 - Add recipes to drinks, with different stations and minigames required for each step:
   - e.g Coffee: Tamp coffee with pressure meter rising until you release in green zone, grind the beans, extract the coffee, latte art
     Tea: Measure tea leaves, leave to steep but you have to get the time right
     hot chocolate: measure powder, whisk, cover with whipped cream
     they could also be more generic minigames but these are ideas
 - Expand customer logic
   - Some might want takeout straight from the counter, some might sit at the table for a while
   - Maybe they come in in groups
   - 