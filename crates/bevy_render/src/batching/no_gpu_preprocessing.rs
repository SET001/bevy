//! Batching functionality when GPU preprocessing isn't in use.

use bevy_derive::{Deref, DerefMut};
use bevy_ecs::system::{Query, Res, ResMut, Resource, StaticSystemParam};
use smallvec::{smallvec, SmallVec};
use wgpu::BindingResource;

use crate::{
    render_phase::{
        BinnedPhaseItem, BinnedRenderPhase, BinnedRenderPhaseBatch, CachedRenderPipelinePhaseItem,
        SortedPhaseItem, SortedRenderPhase,
    },
    render_resource::{GpuArrayBuffer, GpuArrayBufferable},
    renderer::{RenderDevice, RenderQueue},
};

use super::{GetBatchData, GetFullBatchData};

/// The GPU buffers holding the data needed to render batches.
///
/// For example, in the 3D PBR pipeline this holds `MeshUniform`s, which are the
/// `BD` type parameter in that mode.
#[derive(Resource, Deref, DerefMut)]
pub struct BatchedInstanceBuffer<BD>(pub GpuArrayBuffer<BD>)
where
    BD: GpuArrayBufferable + Sync + Send + 'static;

impl<BD> BatchedInstanceBuffer<BD>
where
    BD: GpuArrayBufferable + Sync + Send + 'static,
{
    /// Creates a new buffer.
    pub fn new(render_device: &RenderDevice) -> Self {
        BatchedInstanceBuffer(GpuArrayBuffer::new(render_device))
    }

    /// Returns the binding of the buffer that contains the per-instance data.
    ///
    /// If we're in the GPU instance buffer building mode, this buffer needs to
    /// be filled in via a compute shader.
    pub fn instance_data_binding(&self) -> Option<BindingResource> {
        self.binding()
    }
}

/// Batch the items in a sorted render phase, when GPU instance buffer building
/// isn't in use. This means comparing metadata needed to draw each phase item
/// and trying to combine the draws into a batch.
pub fn batch_and_prepare_sorted_render_phase<I, GBD>(
    batched_instance_buffer: ResMut<BatchedInstanceBuffer<GBD::BufferData>>,
    mut views: Query<&mut SortedRenderPhase<I>>,
    param: StaticSystemParam<GBD::Param>,
) where
    I: CachedRenderPipelinePhaseItem + SortedPhaseItem,
    GBD: GetBatchData,
{
    let system_param_item = param.into_inner();

    // We only process CPU-built batch data in this function.
    let batched_instance_buffer = batched_instance_buffer.into_inner();

    for mut phase in &mut views {
        super::batch_and_prepare_sorted_render_phase::<I, GBD>(&mut phase, |item| {
            let (buffer_data, compare_data) =
                GBD::get_batch_data(&system_param_item, item.entity())?;
            let buffer_index = batched_instance_buffer.push(buffer_data);

            let index = buffer_index.index;
            *item.batch_range_mut() = index..index + 1;
            *item.dynamic_offset_mut() = buffer_index.dynamic_offset;

            compare_data
        });
    }
}

/// Creates batches for a render phase that uses bins, when GPU batch data
/// building isn't in use.
pub fn batch_and_prepare_binned_render_phase<BPI, GFBD>(
    mut buffer: ResMut<BatchedInstanceBuffer<GFBD::BufferData>>,
    mut views: Query<&mut BinnedRenderPhase<BPI>>,
    param: StaticSystemParam<GFBD::Param>,
) where
    BPI: BinnedPhaseItem,
    GFBD: GetFullBatchData,
{
    let system_param_item = param.into_inner();

    for mut phase in &mut views {
        let phase = &mut *phase; // Borrow checker.

        // Prepare batchables.

        for key in &phase.batchable_keys {
            let mut batch_set: SmallVec<[BinnedRenderPhaseBatch; 1]> = smallvec![];
            for &entity in &phase.batchable_values[key] {
                let Some(buffer_data) = GFBD::get_binned_batch_data(&system_param_item, entity)
                else {
                    continue;
                };
                let instance = buffer.push(buffer_data);

                // If the dynamic offset has changed, flush the batch.
                //
                // This is the only time we ever have more than one batch per
                // bin. Note that dynamic offsets are only used on platforms
                // with no storage buffers.
                if !batch_set.last().is_some_and(|batch| {
                    batch.instance_range.end == instance.index
                        && batch.dynamic_offset == instance.dynamic_offset
                }) {
                    batch_set.push(BinnedRenderPhaseBatch {
                        representative_entity: entity,
                        instance_range: instance.index..instance.index,
                        dynamic_offset: instance.dynamic_offset,
                    });
                }

                if let Some(batch) = batch_set.last_mut() {
                    batch.instance_range.end = instance.index + 1;
                }
            }

            phase.batch_sets.push(batch_set);
        }

        // Prepare unbatchables.
        for key in &phase.unbatchable_keys {
            let unbatchables = phase.unbatchable_values.get_mut(key).unwrap();
            for &entity in &unbatchables.entities {
                let Some(buffer_data) = GFBD::get_binned_batch_data(&system_param_item, entity)
                else {
                    continue;
                };
                let instance = buffer.push(buffer_data);
                unbatchables.buffer_indices.add(instance);
            }
        }
    }
}

/// Writes the instance buffer data to the GPU.
pub fn write_batched_instance_buffer<GBD>(
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
    mut cpu_batched_instance_buffer: ResMut<BatchedInstanceBuffer<GBD::BufferData>>,
) where
    GBD: GetBatchData,
{
    cpu_batched_instance_buffer.write_buffer(&render_device, &render_queue);
}
