use bevy::prelude::*;

// Constants for time management
pub const SECONDS_PER_DAY: f32 = 86400.0; // 24 hours * 60 minutes * 60 seconds
pub const DAY_CYCLE_DURATION: f32 = 1800.0; // 30 real minutes = 1 game day
pub const TIME_SCALE: f32 = SECONDS_PER_DAY / DAY_CYCLE_DURATION; // 48x speed multiplier
pub const STARTING_TIME: f32 = 56000.0; // 10am in seconds (10 * 3600)
pub const SUN_DISTANCE: f32 = 5000.0; // Distance from origin for sun position

/// Resource to track the current time of day
#[derive(Resource, Debug)]
pub struct TimeOfDay {
    pub seconds: f32, // 0.0 to 86400.0 (seconds since midnight)
}

impl Default for TimeOfDay {
    fn default() -> Self {
        Self {
            seconds: STARTING_TIME,
        }
    }
}

impl TimeOfDay {
    /// Get the current hour (0.0 to 24.0)
    pub fn hours(&self) -> f32 {
        self.seconds / 3600.0
    }

    /// Get formatted time string (HH:MM AM/PM)
    pub fn format_time(&self) -> String {
        let total_minutes = (self.seconds / 60.0) as u32;
        let hours = total_minutes / 60;
        let minutes = total_minutes % 60;

        let (display_hour, period) = if hours == 0 {
            (12, "AM")
        } else if hours < 12 {
            (hours, "AM")
        } else if hours == 12 {
            (12, "PM")
        } else {
            (hours - 12, "PM")
        };

        format!("{:02}:{:02} {}", display_hour, minutes, period)
    }
}

/// Marker component for the sun/moon directional light
#[derive(Component)]
pub struct Sun;

/// System to update the time of day
pub fn update_time_of_day(mut time_of_day: ResMut<TimeOfDay>, time: Res<Time>) {
    time_of_day.seconds += time.delta_secs() * TIME_SCALE;

    // Wrap around at end of day
    if time_of_day.seconds >= SECONDS_PER_DAY {
        time_of_day.seconds -= SECONDS_PER_DAY;
    }
}

/// System to update the sun's position based on time of day
pub fn update_sun_transform(
    time_of_day: Res<TimeOfDay>,
    mut sun_query: Query<&mut Transform, With<Sun>>,
) {
    let Ok(mut transform) = sun_query.single_mut() else {
        return;
    };

    // Calculate sun angle based on time
    // At 0 seconds (midnight): angle = -PI (sun below horizon, pointing down)
    // At 43200 seconds (noon): angle = 0 (sun overhead)
    // At 86400 seconds (next midnight): angle = PI (sun below horizon again)
    let angle =
        (time_of_day.seconds / SECONDS_PER_DAY) * std::f32::consts::TAU - std::f32::consts::PI;

    // Position sun on an arc (rotate around X-axis for east-west movement)
    // Y component: negative when sun is below horizon, positive when above
    // Z component: determines east/west position
    let sun_pos = Vec3::new(0.0, angle.sin() * SUN_DISTANCE, angle.cos() * SUN_DISTANCE);

    // Point the light at the origin
    *transform = Transform::from_translation(sun_pos).looking_at(Vec3::ZERO, Vec3::Y);
}

/// System to update the sun's color and brightness based on time of day
pub fn update_sun_properties(
    time_of_day: Res<TimeOfDay>,
    mut sun_query: Query<&mut DirectionalLight, With<Sun>>,
) {
    let Ok(mut light) = sun_query.single_mut() else {
        return;
    };

    light.color = get_sun_color(time_of_day.seconds);
    light.illuminance = get_sun_illuminance(time_of_day.seconds);
}

/// Get the sun color based on time of day
fn get_sun_color(_seconds: f32) -> Color {
    // Using warm sunlight color that works well with atmosphere
    // Atmosphere will handle scattering and color changes based on sun angle
    Color::srgb(1.0, 0.95, 0.9)
}

// Unused match for future dynamic color changes
fn _get_sun_color_dynamic(seconds: f32) -> Color {
    let hour = seconds / 3600.0;

    match hour {
        // Night (midnight to 6am): Blue moonlight
        h if h < 6.0 => Color::srgb(0.3, 0.3, 0.5),

        // Dawn (6am to 8am): Transition from blue to warm yellow
        h if h < 8.0 => {
            let t = (h - 6.0) / 2.0; // 0.0 to 1.0
            Color::srgb(
                0.3 + t * 0.7,  // 0.3 -> 1.0
                0.3 + t * 0.65, // 0.3 -> 0.95
                0.5 + t * 0.4,  // 0.5 -> 0.9
            )
        }

        // Day (8am to 5pm): Bright warm white
        h if h < 17.0 => Color::srgb(1.0, 0.95, 0.9),

        // Dusk (5pm to 7pm): Transition from yellow to orange to blue
        h if h < 19.0 => {
            let t = (h - 17.0) / 2.0; // 0.0 to 1.0
            Color::srgb(
                1.0 - t * 0.7,   // 1.0 -> 0.3
                0.95 - t * 0.65, // 0.95 -> 0.3
                0.9 - t * 0.4,   // 0.9 -> 0.5
            )
        }

        // Night (7pm to midnight): Blue moonlight
        _ => Color::srgb(0.3, 0.3, 0.5),
    }
}

/// Get the sun illuminance (brightness) based on time of day
fn get_sun_illuminance(_seconds: f32) -> f32 {
    // Using realistic sunlight illuminance for atmosphere system
    // Bevy's lux::RAW_SUNLIGHT is approximately 128,000 lux
    // Using a slightly lower value that still works well with atmosphere
    100_0.0
}
