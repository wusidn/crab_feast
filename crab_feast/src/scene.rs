use crate::level::{LevelBlock, LevelLook, spawn_static_level};
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
use crate::input::{ControlInputPlugin, ControlSet, Facing, LookAxis, LookController, MovementController};
use crate::state::SessionMode;

pub struct ScenePlugin;

/// 玩家实体（角色模型挂在这个实体下面，动画预览切换模型时只替换子节点）
#[derive(Component)]
pub struct Avatar;

/// 与关卡无关的场景装饰（灯光等）：退出 `Game` 时按这个标记统一清掉。
#[derive(Component, Default, Clone)]
pub struct SceneDecor;

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
            // 关卡是**静态场景**：两端各自按同一份 `LEVEL` 建实体，客户端不再等复制。
            // 这里只负责把本地建出来的 `LevelBlock` 画出来；几何与碰撞体在
            // `spawn_static_level` 里已经一起建好
            .add_systems(Update, render_level_blocks);
    }
}
impl ScenePlugin {
    fn setup(mut commands: Commands, game_camera: Res<GameCamera>, mode: Res<SessionMode>) {
        // 与关卡内容无关的装饰与照明。
        // 退出 `Game` 后按 `SceneDecor` 清理，重进时重新生成
        commands.spawn_scene_list(bsn_list! {
                // Mesh3d(asset_value(Circle::new(4.0)))
                // MeshMaterial3d<StandardMaterial>(asset_value(Color::WHITE))
                // Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
                // --
                PointLight {
                    shadow_maps_enabled: true,
                }
                Transform::from_xyz(4.0, 8.0, 4.0)
                SceneDecor
                --
                // 全局光照：从右上方照下来，比 Bevy 默认的日光（10_000 lux）暗一级
                DirectionalLight {
                    color: Color::WHITE,
                    illuminance: 300.0,
                    shadow_maps_enabled: true,
                }
                Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_4) * Quat::from_rotation_x(-std::f32::consts::FRAC_PI_4))
                SceneDecor
            });

        // 静态场景：两端各自构建，客户端这里拿到的就是本地物理世界里的真实几何
        // （Static 刚体 + cuboid 碰撞体），不是"只给射线用"的探针
        spawn_static_level(&mut commands);

        // 相机靠 `Avatar` 标记找角色，不再需要这里的实体句柄：一旦挂成子节点，
        // 角色转身就会把相机一起拧走，朝向又耦合回去了
        //
        // 只有**本地**这一局才自己生成角色：联机模式下的角色由服务器复制下来
        // （见 net.rs），客户端不能再建一个，否则两边都写同一个角色的 Transform。
        //
        // 注意这里看的是运行时 `SessionMode`，不看编译特性：pc 二进制带 `net`，
        // 但它也能从大厅点「本地游戏」进来，那条路必须有自己的角色。
        if matches!(*mode, SessionMode::Local) {
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
        }

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

/// 把 `LevelBlock` 画出来：几何与碰撞体在 `spawn_static_level` 里已经建好，
/// 这里只补视觉体（mesh + 材质）。
///
/// 两端都走这一个系统：服务端没有渲染层，这个查询在那边为空，自然什么都不做。
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
        // size，画面和物理不会差出一截。碰撞体不在这里加——它属于静态场景本身，
        // 由 `spawn_static_level` 建实体时就一起挂上
        let mut block_entity = commands.entity(entity);
        block_entity.insert((
            Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
            MeshMaterial3d(material),
        ));
    }
}
