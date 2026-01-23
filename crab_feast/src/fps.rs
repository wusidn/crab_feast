use bevy::prelude::*;
use crate::assets::DefaultAssets;

/// FPS显示资源
#[derive(Resource)]
struct FpsData {
    /// 当前FPS值
    fps: f32,
    /// 上次更新显示的时间
    last_update_time: f64,
    /// 存储最近的帧时间戳，使用固定大小数组提高性能
    frame_timestamps: [Option<f64>; 200], // 足够存储200帧的时间戳
    /// 数组当前索引
    current_index: usize,
    /// 平滑因子，用于稳定FPS显示
    smooth_factor: f32,
    /// 是否已初始化FPS值的标志
    initialized: bool,
    /// 是否启用平滑因子
    use_smoothing: bool,
}

// 手动实现Default trait，因为长度大于32的数组不支持自动派生
impl Default for FpsData {
    fn default() -> Self {
        Self {
            fps: 60.0, // 使用更合理的默认初始值
            last_update_time: 0.0,
            // 使用std::array::from_fn初始化大数组
            frame_timestamps: std::array::from_fn(|_| None),
            current_index: 0,
            // 平滑因子（稍微提高一点，让显示更稳定）
            smooth_factor: 0.3,
            // 初始化为未初始化状态
            initialized: false,
            // 初始不启用平滑因子
            use_smoothing: false,
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
    assets: Res<DefaultAssets>,
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
                    font: assets.font.clone(),
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
        // 使用更合理的初始值
        fps_data.fps = 60.0;
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
        let time_window_ago = current_time - 1.0; // 使用1秒的时间窗口收集数据
        
        // 收集最近一秒内的有效时间戳
        let mut valid_timestamps = Vec::new();
        for &timestamp in fps_data.frame_timestamps.iter() {
            if let Some(ts) = timestamp {
                if ts >= time_window_ago {
                    valid_timestamps.push(ts);
                }
            }
        }
        
        // 检查是否启用平滑因子：降低启用门槛
        if !fps_data.use_smoothing {
            // 降低要求：只需要6个时间戳（5帧）
            if valid_timestamps.len() >= 6 {
                // 按时间排序
                valid_timestamps.sort_by(|a, b| a.partial_cmp(b).unwrap());
                
                // 取最近的6个时间戳（用于计算5帧）
                let recent_timestamps = valid_timestamps[(valid_timestamps.len() - 6)..].to_vec();
                
                // 计算最近5帧的FPS值
                let mut recent_fps = Vec::new();
                for i in 1..recent_timestamps.len() {
                    let frame_delta = recent_timestamps[i] - recent_timestamps[i - 1];
                    if frame_delta > 0.0 {
                        recent_fps.push((1.0 / frame_delta) as f32);
                    }
                }
                
                // 检查是否有足够的FPS数据
                if recent_fps.len() >= 5 {
                    // 找出最小值和最大值
                    let min_fps = recent_fps.iter().fold(f32::INFINITY, |min, &fps| fps.min(min));
                    let max_fps = recent_fps.iter().fold(f32::NEG_INFINITY, |max, &fps| fps.max(max));
                    
                    // 计算波动百分比
                    if max_fps > 0.0 {
                        let fluctuation = (max_fps - min_fps) / max_fps * 100.0;
                        
                        // 提高波动阈值，让平滑因子更容易启用
                        if fluctuation < 50.0 {
                            fps_data.use_smoothing = true;
                            info!("FPS smoothing enabled. Fluctuation: {:.2}%", fluctuation);
                        }
                    }
                }
            }
        }
        
        // 计算平均FPS - 修复：无论时间窗口是否完整，都使用所有有效时间戳
        let new_fps = if valid_timestamps.len() >= 2 {
            // 按时间排序
            valid_timestamps.sort_by(|a, b| a.partial_cmp(b).unwrap());
            
            // 使用实际收集到的帧间隔计算FPS
            let first_time = valid_timestamps.first().unwrap();
            let last_time = valid_timestamps.last().unwrap();
            let total_time = last_time - first_time;
            let frame_count = valid_timestamps.len() - 1;
            
            // 只要总时间 > 0，就使用所有有效数据计算FPS
            if total_time > 0.0 {
                // 计算FPS并限制在合理范围内
                (frame_count as f64 / total_time) as f32
            } else {
                fps_data.fps // 保持当前值
            }
        } else {
            // 没有足够的有效帧时，保持当前值
            fps_data.fps
        };
        
        // 限制FPS值在合理范围内
        let clamped_fps = new_fps.clamp(10.0, 240.0);
        
        // 应用平滑因子（仅当启用时）
        if fps_data.use_smoothing {
            fps_data.fps = fps_data.fps * fps_data.smooth_factor + clamped_fps * (1.0 - fps_data.smooth_factor);
        } else {
            // 不使用平滑因子时，直接使用新计算的FPS值
            fps_data.fps = clamped_fps;
        }
        
        // 更新显示
        for mut text in text_query.iter_mut() {
            text.0 = format!("FPS: {:.2}", fps_data.fps);
        }
        
        // 记录更新时间
        fps_data.last_update_time = current_time;
    }
}