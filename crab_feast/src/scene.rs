use avian3d::prelude::*;
use bevy::{
    asset::RenderAssetUsages,
    image::ImageSampler,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};

use crate::GameState;
use crate::anim::AnimationSet;
use crate::camera::{CameraRig, GameCamera};
use crate::input::{
    ControlInputPlugin, ControlSet, Facing, LookAxis, LookController, MovementController,
};

pub struct ScenePlugin;

/// 玩家实体（角色模型挂在这个实体下面，动画预览切换模型时只替换子节点）
#[derive(Component)]
pub struct Avatar;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ControlInputPlugin)
            .add_plugins((PhysicsPlugins::default(), PhysicsDebugPlugin))
            .configure_sets(
                Update,
                // 相机定机位 → 摇杆换算世界方向 → 角色限速转身，这一串跑完才轮到
                // 动画系统拿本帧的朝向误差去选姿态，否则转向权重会慢一帧
                (ControlSet::Camera, ControlSet::Move, ControlSet::Facing)
                    .chain()
                    .before(AnimationSet::Drive),
            )
            .add_systems(OnEnter(GameState::Game), Self::setup);
    }
}
impl ScenePlugin {
    fn setup(
        mut commands: Commands,
        game_camera: Res<GameCamera>,
        mut images: ResMut<Assets<Image>>,
        mut materials: ResMut<Assets<StandardMaterial>>,
    ) {
        let ground_size = 30.0;
        let ground_height = 0.1;
        // 地面宽 60 米、烘 60×60 格 → 一格一米
        let checker_cells = (ground_size * 2.0) as usize;
        let checker_material = materials.add(StandardMaterial {
            base_color_texture: Some(checkerboard(&mut images, checker_cells)),
            ..default()
        });

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
            // 全局光照：从右上方照下来，比 Bevy 默认的日光（10_000 lux）暗一级
            DirectionalLight {
                color: Color::WHITE,
                illuminance: 300.0,
                shadow_maps_enabled: true,
            }
            Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_4) * Quat::from_rotation_x(-std::f32::consts::FRAC_PI_4))
            --
            @ground(ground_size, ground_height, checker_material)
        });

        // 相机靠 `Avatar` 标记找角色，不再需要这里的实体句柄：一旦挂成子节点，
        // 角色转身就会把相机一起拧走，朝向又耦合回去了
        commands.spawn((
            Transform::from_xyz(0.0, 2.0, 0.0),
            RigidBody::Dynamic,
            // Avian 的 capsule 用「半径 + 不含半球的原柱长度」，
            // rapier 的 capsule_y(half_height, radius) 等价于 capsule(0.2, 1.0)
            Collider::capsule(0.2, 1.0),
            // DebugRender::default().with_collider_color(Color::WHITE),
            Restitution::new(0.3),
            LinearDamping(0.3),
            AngularDamping(0.3),
            // SweptCcd::default(),
            LockedAxes::ROTATION_LOCKED,
            LinearVelocity::ZERO,
            MovementController::default(),
            // 角色不再被 look 输入直接拧头：它只拿一个目标朝向，按角速度追
            Facing::default(),
            InheritedVisibility::default(),
            Visibility::Visible,
            Avatar,
        ));

        // 相机不再是角色的子节点：挂着就会被角色的旋转带走，朝向永远耦合。
        // 现在它自己存一份偏航/俯仰（look 输入积分），机位每帧由 orbit_camera 重算；
        // look_system 顺手写的那个 rotation 会被 orbit_camera 覆盖，只当中间量。
        commands.entity(game_camera.0).insert((
            CameraRig::default(),
            // 初始机位在角色背后（模型面向 +Z）；进 Game 后每帧由 orbit_camera 重算
            Transform::from_xyz(0.0, 1.3, -5.0).looking_at(Vec3::ZERO, Vec3::Y),
            LookController {
                axis: LookAxis::YawAndPitch,
                ..Default::default()
            },
        ));
    }
}

/// 黑白格地板贴图，`cells` 是每边的格子数。
///
/// 格子直接烘进纹理而不靠平铺采样器：Bevy 的标准材质没有 tiling uniform，
/// Cuboid 每个面的 UV 又只走 [0,1]，想控住「一格一米」只能把格数做进像素里。
/// Nearest 过滤保留硬边，脚步到底有没有真的在地面上走一眼就能看出来。
fn checkerboard(images: &mut Assets<Image>, cells: usize) -> Handle<Image> {
    const CELL_PX: usize = 8;
    let side = (cells * CELL_PX) as u32;

    let mut pixels = vec![0u8; (side * side * 4) as usize];
    for (index, pixel) in pixels.chunks_mut(4).enumerate() {
        let column = (index as u32 % side / CELL_PX as u32) as usize;
        let row = (index as u32 / side / CELL_PX as u32) as usize;
        let shade: u8 = if (column + row).is_multiple_of(2) {
            235
        } else {
            28
        };
        pixel.copy_from_slice(&[shade, shade, shade, 255]);
    }

    let mut image = Image::new(
        Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::nearest();
    images.add(image)
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

/// 地面：渲染 mesh + 碰撞体 + 固定刚体，表面贴黑白格
fn ground(size: f32, height: f32, material: Handle<StandardMaterial>) -> impl Scene {
    let debug_color = DebugRender::default().with_collider_color(Color::srgb(0.0, 0.0, 0.0));
    bsn! {
        Transform::from_xyz(0.0, -height / 2.0, 0.0)
        // mesh 尺寸与碰撞体一致：两者重合就不会有 z-fighting，也不用再摆一个偏移的平面
        Mesh3d(asset_value(Cuboid::new(size * 2.0, height * 2.0, size * 2.0)))
        MeshMaterial3d<StandardMaterial>(material)
        ~Collider::cuboid(size * 2.0, height * 2.0, size * 2.0)
        // Avian 不会把「只有 Collider 没有 RigidBody」的实体当成静态体，
        // 缺了这行地面就是个纯渲染/纯调试物件，物体会直接穿过去
        ~RigidBody::Static
        debug_color
    }
}
