use bevy::prelude::*;

// 根移动组件 - 记录根骨骼的运动数据
#[derive(Component)]
pub struct RootMotion {
    pub initial_transform: Transform,        // 动画开始时的初始变换
    pub accumulated_translation: Vec3,      // 累计位移
    pub accumulated_rotation: Quat,         // 累计旋转
    pub is_active: bool,                    // 是否激活状态
    pub animation_start_time: f64,         // 动画开始时间
}

impl Default for RootMotion {
    fn default() -> Self {
        Self {
            initial_transform: Transform::IDENTITY,
            accumulated_translation: Vec3::ZERO,
            accumulated_rotation: Quat::IDENTITY,
            is_active: false,
            animation_start_time: 0.0,
        }
    }
}

// 中间层组件 - 用于抵消根骨骼运动
#[derive(Component)]
pub struct MotionIntermediateLayer {
    pub root_bone_entity: Entity,           // 对应的根骨骼实体
}

// 根骨骼标记组件
#[derive(Component)]
pub struct RootBone;

// 根移动系统插件
pub struct RootMotionPlugin;

impl Plugin for RootMotionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                activate_root_motion_on_animation_start,
                detect_root_bone_movement,
                apply_intermediate_layer_motion,
                sync_player_position,
                cleanup_motion_after_animation,
            ).chain(),
        );
    }
}

// 系统1: 动画开始时激活根移动
fn activate_root_motion_on_animation_start(
    mut commands: Commands,
    time: Res<Time>,
    animation_players: Query<&AnimationPlayer, Changed<AnimationPlayer>>,
    root_bones: Query<Entity, With<RootBone>>,
) {
    for animation_player in &animation_players {
        // 检测动画状态变化（这里简化处理，实际需要更精确的检测）
        for root_bone_entity in &root_bones {
            if let Ok(mut entity_commands) = commands.get_entity(root_bone_entity) {
                entity_commands.insert(RootMotion {
                    initial_transform: Transform::IDENTITY, // 会在下一帧更新
                    accumulated_translation: Vec3::ZERO,
                    accumulated_rotation: Quat::IDENTITY,
                    is_active: true,
                    animation_start_time: time.elapsed_secs_f64(),
                });
            }
        }
    }
}

// 系统2: 检测根骨骼运动
fn detect_root_bone_movement(
    time: Res<Time>,
    mut root_bones: Query<(&mut RootMotion, &GlobalTransform), With<RootBone>>,
) {
    for (mut root_motion, global_transform) in &mut root_bones {
        if root_motion.is_active {
            // 如果是第一帧，记录初始位置
            if root_motion.initial_transform == Transform::IDENTITY {
                root_motion.initial_transform = Transform::from_matrix(global_transform.to_matrix());
            }
            
            // 计算相对于初始位置的位移
            let current_transform = Transform::from_matrix(global_transform.to_matrix());
            let delta_translation = current_transform.translation - root_motion.initial_transform.translation;
            let delta_rotation = current_transform.rotation * root_motion.initial_transform.rotation.inverse();
            
            root_motion.accumulated_translation = delta_translation;
            root_motion.accumulated_rotation = delta_rotation;
        }
    }
}

// 系统3: 应用中间层反向运动
fn apply_intermediate_layer_motion(
    mut intermediate_layers: Query<(&MotionIntermediateLayer, &mut Transform)>,
    root_motions: Query<&RootMotion>,
) {
    for (intermediate_layer, mut transform) in &mut intermediate_layers {
        if let Ok(root_motion) = root_motions.get(intermediate_layer.root_bone_entity) {
            if root_motion.is_active {
                // 中间层进行反向运动来抵消根骨骼位移
                transform.translation = -root_motion.accumulated_translation;
                transform.rotation = root_motion.accumulated_rotation.inverse();
            }
        }
    }
}

// 系统4: 同步玩家位置
fn sync_player_position(
    mut players: Query<&mut Transform, (Without<MotionIntermediateLayer>, Without<RootBone>)>,
    root_motions: Query<&RootMotion, With<RootBone>>,
) {
    for mut player_transform in &mut players {
        for root_motion in &root_motions {
            if root_motion.is_active {
                // 玩家实体跟随根骨骼的实际运动
                player_transform.translation += root_motion.accumulated_translation;
                player_transform.rotation = root_motion.accumulated_rotation * player_transform.rotation;
            }
        }
    }
}

// 系统5: 动画完成后清理运动
fn cleanup_motion_after_animation(
    time: Res<Time>,
    mut root_motions: Query<&mut RootMotion>,
    animation_players: Query<&AnimationPlayer>,
) {
    for mut root_motion in &mut root_motions {
        if root_motion.is_active {
            // 简化处理：如果动画播放超过5秒，认为完成（实际需要更精确的检测）
            if time.elapsed_secs_f64() - root_motion.animation_start_time > 5.0 {
                root_motion.is_active = false;
                root_motion.accumulated_translation = Vec3::ZERO;
                root_motion.accumulated_rotation = Quat::IDENTITY;
                info!("Root motion cleanup completed");
            }
        }
    }
}

// 工具函数：为角色设置根移动系统
pub fn setup_root_motion_for_character(
    commands: &mut Commands,
    player_entity: Entity,
    root_bone_entity: Entity,
) -> Entity {
    // 为根骨骼添加标记
    commands.entity(root_bone_entity).insert(RootBone);
    
    // 创建中间层实体，使用 ChildOf 组件建立层级关系
    let intermediate_entity = commands.spawn((
        Name::new("MotionIntermediateLayer"),
        MotionIntermediateLayer {
            root_bone_entity,
        },
        Transform::IDENTITY,
        GlobalTransform::IDENTITY,
        ChildOf(player_entity), // 中间层是玩家的子级
    )).id();
    
    // 为根骨骼添加 ChildOf 组件，使其成为中间层的子级
    commands.entity(root_bone_entity).insert(ChildOf(intermediate_entity));
    
    intermediate_entity
}

// 工具函数：手动激活根移动
pub fn activate_root_motion(
    commands: &mut Commands,
    root_bone_entity: Entity,
    initial_transform: Transform,
) {
    if let Ok(mut entity_commands) = commands.get_entity(root_bone_entity) {
        entity_commands.insert(RootMotion {
            initial_transform,
            accumulated_translation: Vec3::ZERO,
            accumulated_rotation: Quat::IDENTITY,
            is_active: true,
            animation_start_time: 0.0, // 会在系统中更新
        });
    }
}