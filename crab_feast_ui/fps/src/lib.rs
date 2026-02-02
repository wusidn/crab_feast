use bevy::prelude::*;

/// FPS显示资源
#[derive(Resource)]
struct FpsData {
    /// 当前FPS值
    fps: f32,
    /// 上次更新显示的时间
    last_update_time: f64,
    /// 存储最近的帧时间戳，使用固定大小数组提高性能
    frame_timestamps: [Option<f64>; 100], // 足够存储100帧的时间戳
    /// 数组当前索引
    current_index: usize,
    /// 是否已初始化FPS值的标志
    initialized: bool,
}

// 手动实现Default trait，因为长度大于32的数组不支持自动派生
impl Default for FpsData {
    fn default() -> Self {
        Self {
            fps: 0.0,
            last_update_time: 0.0,
            // 使用std::array::from_fn初始化大数组
            frame_timestamps: std::array::from_fn(|_| None),
            current_index: 0,
            // 初始化为未初始化状态
            initialized: false,
        }
    }
}

#[derive(Component)]
struct FpsText;

pub struct FPSPlugin;

impl Plugin for FPSPlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(FpsData::default())
            .add_systems(Startup, fps_setup)
            .add_systems(Update, update_fps);
    }
}

fn fps_setup(
    mut commands: Commands,
) { 
    // FPS显示节点
    commands.spawn((
        Node{
            width: Val::Auto,
            height: Val::Px(50.0),
            justify_content: JustifyContent::FlexStart,
            align_items: AlignItems::FlexStart,
            padding: UiRect::all(Val::Px(10.0)),
            ..Default::default()
        },
        children![
            (
                Text("FPS: --".to_string()), // 初始显示占位符
                TextFont{
                    font_size: 18.0,
                    ..Default::default()
                },
                TextColor(Color::WHITE),
                TextLayout {
                    justify: Justify::Left,
                    linebreak: LineBreak::AnyCharacter,
                },
                FpsText,
            )
        ]
    ));
}

/// 更新FPS数据并显示
fn update_fps(
    time: Res<Time>,
    mut fps_data: ResMut<FpsData>,
    mut text_query: Query<&mut Text, With<FpsText>>,
) {
    let current_time = time.elapsed_secs_f64();
    
    // 先将current_index存储到局部变量，避免可变和不可变借用冲突
    let current_idx = fps_data.current_index;
    
    // 记录当前帧时间戳
    fps_data.frame_timestamps[current_idx] = Some(current_time);
    fps_data.current_index = (current_idx + 1) % fps_data.frame_timestamps.len();

    // 初始化阶段
    if !fps_data.initialized {
        // 使用更合理的初始值（当前帧的时间间隔）
        fps_data.fps = time.delta_secs() / 1.0;
        fps_data.initialized = true;
        
        // 立即更新显示
        for mut text in text_query.iter_mut() {
            text.0 = format!("FPS: {:.2}", fps_data.fps);
        }
        
        // 初始化last_update_time
        fps_data.last_update_time = current_time;
        return;
    }
    
    // 每0.1秒更新一次FPS显示
    if current_time - fps_data.last_update_time >= 0.1 {
        let time_window_ago = current_time - 0.2; // 200ms时间窗口
        let mut first_time: Option<f64> = None;
        let mut last_time: Option<f64> = None;
        let mut frame_count = 0;
        for &timestamp in fps_data.frame_timestamps.iter() {
            if let Some(ts) = timestamp {
                if ts >= time_window_ago {
                    frame_count += 1;
                    match first_time {
                        Some(first) => {
                            if ts < first {
                                first_time = Some(ts);
                            }
                        },
                        None => first_time = Some(ts),
                    }

                    match last_time {
                        Some(last) => {
                            if ts > last {
                                last_time = Some(ts);
                            }
                        },
                        None => last_time = Some(ts),
                    }
                }
            }
        }

        let new_fps = if let (Some(first_time), Some(last_time)) = (first_time, last_time) {
            if frame_count > 1 {
                // 使用实际收集到的帧间隔计算FPS
                let total_time = last_time - first_time;
                if total_time > 0.0 {
                    ((frame_count - 1) as f64 / total_time) as f32
                } else {
                    0.0
                }
            } else {
                0.0
            }
        } else {
            0.0
        };
        
        // 直接使用新计算的FPS值
        fps_data.fps = new_fps;
        
        // 更新显示
        for mut text in text_query.iter_mut() {
            text.0 = format!("FPS: {:.2}", fps_data.fps);
        }
        
        // 记录更新时间
        fps_data.last_update_time = current_time;
    }
}