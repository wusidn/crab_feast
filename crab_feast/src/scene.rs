use avian3d::prelude::*;
use bevy::{
    asset::RenderAssetUsages,
    image::ImageSampler,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use crab_feast_library::level::{LevelBlock, LevelLook};

use crate::GameState;
use crate::anim::AnimationSet;
use crate::camera::{CameraRig, GameCamera};
use crate::input::{ControlInputPlugin, ControlSet, LookAxis, LookController};
#[cfg(not(feature = "net"))]
use crate::input::{Facing, MovementController};

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
            .add_systems(OnEnter(GameState::Game), Self::setup)
            // 关卡内容由权威侧（联网 = 服务端）spawn 成实体，这里只负责把
            // 复制下来的 `LevelBlock` 画出来，不决定「哪里有方块」
            .add_systems(Update, render_level_blocks);
    }
}
impl ScenePlugin {
    fn setup(mut commands: Commands, game_camera: Res<GameCamera>) {
        // 与关卡内容无关的装饰与照明
        commands.spawn_scene_list(bsn_list! {
            // Mesh3d(asset_value(Circle::new(4.0)))
            // MeshMaterial3d<StandardMaterial>(asset_value(Color::WHITE))
            // Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
            // --
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
        });

        // 单机模式没有服务器，本地就是权威：关卡实体在这儿 spawn。
        // 联网模式下一条都不写——关卡由服务端实体复制下来（见 `render_level_blocks`）
        #[cfg(not(feature = "net"))]
        spawn_local_level(&mut commands);

        // 相机靠 `Avatar` 标记找角色，不再需要这里的实体句柄：一旦挂成子节点，
        // 角色转身就会把相机一起拧走，朝向又耦合回去了
        //
        // 联网模式下角色实体由服务器复制下来（见 net.rs），本地不再生成，
        // 否则会和复制实体重复，且两边都对同一个角色写 Transform
        #[cfg(not(feature = "net"))]
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

/// 把 `LevelBlock` 画出来：**只读实体上写了什么**，不参与决定关卡里有什么。
///
/// 联网时这些实体是服务端复制下来的，单机时是本地 spawn 的；两种模式共用这一个
/// 系统，所以「服务端挪了方块、客户端画面没跟上」这种漂移在结构上不会发生。
fn render_level_blocks(
    mut commands: Commands,
    blocks: Query<(Entity, &LevelBlock), Added<LevelBlock>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut cache: Local<Option<(Handle<StandardMaterial>, Handle<StandardMaterial>)>>,
) {
    if blocks.is_empty() {
        return;
    }

    let (ground_material, cube_material) = match cache.as_ref() {
        Some(materials) => materials.clone(),
        None => {
            // 一格一米：格数直接按地面的边长算，不另写常量
            let cells = blocks
                .iter()
                .find(|(_, block)| block.look == LevelLook::Ground)
                .map(|(_, block)| block.size[0].round().max(1.0) as usize)
                .unwrap_or(60);
            let pair = (
                materials.add(StandardMaterial {
                    base_color_texture: Some(checkerboard(&mut images, cells)),
                    ..default()
                }),
                materials.add(StandardMaterial {
                    base_color: Color::srgb_u8(124, 144, 255),
                    ..default()
                }),
            );
            *cache = Some(pair.clone());
            pair
        }
    };

    for (entity, block) in &blocks {
        let size = Vec3::from_array(block.size);
        let material = match block.look {
            LevelLook::Ground => ground_material.clone(),
            LevelLook::Cube => cube_material.clone(),
        };
        // 视觉体就长在 `LevelBlock` 这个实体自己身上：mesh 尺寸与碰撞体是同一份
        // size，画面和物理不会差出一截
        let mut block_entity = commands.entity(entity);
        block_entity.insert((
            Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
            MeshMaterial3d(material),
            Transform::from_translation(Vec3::from_array(block.center)),
        ));
        // 联网模式下客户端不跑物理（角色位置由服务器写），但脚部 IK 每帧要往下打
        // 一条射线量地面；而 `LevelBlock` 的复制名单里没有 `Collider`，复制下来的
        // 关卡实体是个纯视觉体 —— 射线一路打空，IK 只能退回「从脚踝推断」。
        // 这里补一份静态碰撞体，只当射线目标用，不参与任何积分
        #[cfg(feature = "net")]
        block_entity.insert((RigidBody::Static, Collider::cuboid(size.x, size.y, size.z)));
    }
}

/// 单机模式的关卡：本地客户端自己就是权威，实体在这儿 spawn。
///
/// 联网时同样的东西只存在于 `launcher/server/src/level.rs`，客户端一份都不写。
#[cfg(not(feature = "net"))]
fn spawn_local_level(commands: &mut Commands) {
    spawn_block(
        commands,
        Vec3::new(0.0, -0.1, 0.0),
        Vec3::new(60.0, 0.2, 60.0),
        LevelLook::Ground,
    );
    for center in [Vec3::new(0.0, 0.5, 3.0), Vec3::new(1.0, 0.5, 1.0)] {
        spawn_block(commands, center, Vec3::splat(1.0), LevelLook::Cube);
    }
}

#[cfg(not(feature = "net"))]
fn spawn_block(commands: &mut Commands, center: Vec3, size: Vec3, look: LevelLook) {
    let debug_color = match look {
        LevelLook::Ground => Color::srgb(0.0, 0.0, 0.0),
        LevelLook::Cube => Color::srgb(0.6, 0.6, 0.6),
    };
    commands.spawn((
        RigidBody::Static,
        Collider::cuboid(size.x, size.y, size.z),
        Transform::from_translation(center),
        LevelBlock {
            center: center.to_array(),
            size: size.to_array(),
            look,
        },
        // 单机这边是真正在做碰撞的一方，把碰撞体画出来看得到
        DebugRender::default().with_collider_color(debug_color),
    ));
}
