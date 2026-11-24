//! 复制状态的落地：服务端快照 → 本地 `Transform` / 动画参数。
//!
//! 这里只有两条最基础的规则，**没有**插值、**没有**预测、**没有**残差修正：
//!
//! 1. 每帧把最新的 `NetPos` / `NetYaw` 直接写进 `Transform`——位置矫正就是「直接赋值」，
//!    画面永远只有服务端这一条曲线，客户端不模拟、不比对、不产生任何偏置速度；
//! 2. 本地玩家的动画参数（[`LocomotionIntent`] / [`FacingState`]）由复制下来的
//!    `NetMotion` / `NetYaw` 翻译过来：联网模式下没有本地物理去写它们，漏了这步
//!    角色会以 Idle 姿态在地面上滑行。
//!
//! 本地角色和别的玩家走同一条路径（同为 `Kinematic`，位置由快照写）。代价是
//! 输入到画面的延迟 = 一个单程 RTT；换来的是不存在「两套仿真互相拉扯」——
//! 之前的位置修正 / 速度偏置都是在两套仿真之间追残差，快照 60Hz 连着来时
//! 就表现成肉眼可见的抖动和拖拽感。

use avian3d::prelude::{Position, Rotation};
use bevy::prelude::*;
use crab_feast_library::net::{MOVE_SPEED_MPS, NetMotion, NetPlayerId, NetPos, NetYaw};

use crate::input::{FacingState, LocomotionIntent, PlanarMotion, wrap_pi};
use crate::net::LocalPlayer;

/// 所有玩家实体：位置与朝向直接取服务端快照。
///
/// 物理 `Position` / `Rotation` 也要一起写：Avian 的 `position_to_transform` 只要看到
/// 这两个组件变了，就会把 `Transform` 的**平移和旋转**一起覆盖回来。只写 `Transform`、
/// 让 `Position` 停在出生点，碰撞体会留在原地；只写 `Position` 不写 `Rotation`，
/// 朝向又会在下一个物理步被 `Rotation`（单位四元数）冲掉。所以三者同源同值。
///
/// （`transform_to_position` 在 `ScenePlugin` 里是关掉的，不存在反向回灌。）
pub(crate) fn apply_authoritative_pose(
    mut avatars: Query<(
        &NetPos,
        &NetYaw,
        &mut Transform,
        Option<&mut Position>,
        Option<&mut Rotation>,
    )>,
) {
    for (net_pos, net_yaw, mut transform, position, rotation) in &mut avatars {
        let translation = Vec3::from_array(net_pos.0);
        let yaw = Quat::from_rotation_y(net_yaw.0);
        transform.translation = translation;
        transform.rotation = yaw;
        if let Some(mut position) = position {
            position.0 = translation;
        }
        if let Some(mut rotation) = rotation {
            rotation.0 = yaw;
        }
    }
}

/// 本地玩家的动画参数：实测速度 → 档位，意图方向相对朝向 → 混合空间 `yaw`。
///
/// 朝向直接取服务端 `NetYaw`（本体旋转由 [`apply_authoritative_pose`] 写），
/// 所以这里只算「位移方向比脸向偏了多少」，给混合空间的侧移 / 倒退 / 转向档用。
pub(crate) fn drive_net_animation(
    local: Option<Res<LocalPlayer>>,
    mut avatars: Query<(
        &NetPlayerId,
        &NetMotion,
        &NetYaw,
        &mut LocomotionIntent,
        &mut FacingState,
        &mut PlanarMotion,
    )>,
) {
    let Some(local) = local else {
        return;
    };
    for (id, motion, net_yaw, mut intent, mut state, mut planar_motion) in avatars.iter_mut() {
        if id.0 != local.0 {
            continue;
        }
        let planar = Vec3::new(motion.dir[0], 0.0, motion.dir[1]);
        // 主动度是「这一帧有没有自己的输入」：被人推着滑、被平台带走时它是 0，
        // 站定必须清零回到 Idle。只看实测速度的话，人会被搬走的同时原地跑步
        if motion.intent > 1e-3 && planar.length_squared() > 1e-6 {
            let dir = planar.normalize();
            intent.direction = dir;
            intent.magnitude = (motion.speed / MOVE_SPEED_MPS).clamp(0.0, 1.0);
            let target = dir.x.atan2(dir.z);
            state.error_deg = wrap_pi(target - net_yaw.0).to_degrees();
            // 实测位移喂给姿态融合：混合空间的 `yaw` 轴靠它算「位移方向相对脸向」，
            // 没有它转身时腿会继续朝正前方迈（见 [`PlanarMotion`]）
            planar_motion.0 = Vec2::new(dir.x, dir.z) * motion.speed;
        } else {
            intent.direction = Vec3::ZERO;
            intent.magnitude = 0.0;
            state.error_deg = 0.0;
            planar_motion.0 = Vec2::ZERO;
        }
        // 朝向就是服务端的朝向，本地不追、不插值
        state.yaw = net_yaw.0;
        state.yaw_rate = 0.0;
    }
}
