use avian3d::prelude::*;
use bevy::prelude::*;

use crate::GameState;
use crate::camera::GameCamera;
use crate::input::{ControlInputPlugin, LookAxis, LookController, MovementController};

pub struct ScenePlugin;

/// 玩家实体（角色模型挂在这个实体下面，动画预览切换模型时只替换子节点）
#[derive(Component)]
pub struct Avatar;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ControlInputPlugin)
            .add_plugins((
                PhysicsPlugins::default(),
                PhysicsDebugPlugin,
            ))
            .add_systems(OnEnter(GameState::Game), Self::setup);
    }
}
impl ScenePlugin {
    fn setup(mut commands: Commands, game_camera: Res<GameCamera>) {
        let ground_size = 30.0;
        let ground_height = 0.1;

        // 用可复用的 scene 函数组合整个场景（纯视觉 + 带物理均可进 BSN）
        commands.spawn_scene_list(bsn_list! {
            Mesh3d(asset_value(Circle::new(4.0)))
            MeshMaterial3d<StandardMaterial>(asset_value(Color::WHITE))
            Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
            --
            @cube(Vec3::new(0.0, 0.5, 0.0))
            --
            @cube(Vec3::new(1.0, 0.5, 1.0))
            --
            PointLight {
                shadow_maps_enabled: true,
            }
            Transform::from_xyz(4.0, 8.0, 4.0)
            --
            @ground(ground_size, ground_height)
        });

        let player_entity = commands
            .spawn((
                Transform::from_xyz(0.0, 2.0, 0.0),
                RigidBody::Dynamic,
                // Avian 的 capsule 用「半径 + 不含半球的原柱长度」，
                // rapier 的 capsule_y(half_height, radius) 等价于 capsule(0.2, 1.0)
                Collider::capsule(0.2, 1.0),
                DebugRender::default().with_collider_color(Color::WHITE),
                Restitution::new(0.3),
                LinearDamping(0.3),
                AngularDamping(0.3),
                // SweptCcd::default(),
                LockedAxes::ROTATION_LOCKED,
                LinearVelocity::ZERO,
                MovementController::default(),
                LookController {
                    axis: LookAxis::Yaw,
                    ..Default::default()
                },
                InheritedVisibility::default(),
                Visibility::Visible,
                Avatar,
            ))
            .id();

        commands.entity(game_camera.0).insert((
            ChildOf(player_entity),
            Transform::from_xyz(0.0, 1.3, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
            LookController {
                axis: LookAxis::Pitch,
                ..Default::default()
            },
        ));
    }
}

/// 一个带物理的立方体：视觉 + 碰撞体 + 固定刚体
fn cube(translation: Vec3) -> impl Scene {
    // Avian 的 Collider::cuboid 收的是「完整边长」，rapier 收的是半长，数值因此翻倍
    let debug_color = DebugRender::default().with_collider_color(Color::srgb(0.6, 0.6, 0.6));
    bsn! {
        Mesh3d(asset_value(Cuboid::new(1.0, 1.0, 1.0)))
        MeshMaterial3d<StandardMaterial>(asset_value(Color::srgb_u8(124, 144, 255)))
        Transform::from_translation(translation)
        ~Collider::cuboid(1.0, 1.0, 1.0)
        debug_color
        ~RigidBody::Static
    }
}

/// 地面：只有碰撞体，无渲染 mesh
fn ground(size: f32, height: f32) -> impl Scene {
    let debug_color = DebugRender::default().with_collider_color(Color::srgb(0.0, 0.0, 0.0));
    bsn! {
        Transform::from_xyz(0.0, -height / 2.0, 0.0)
        ~Collider::cuboid(size * 2.0, height * 2.0, size * 2.0)
        // Avian 不会把「只有 Collider 没有 RigidBody」的实体当成静态体，
        // 缺了这行地面就是个纯渲染/纯调试物件，物体会直接穿过去
        ~RigidBody::Static
        debug_color
    }
}
