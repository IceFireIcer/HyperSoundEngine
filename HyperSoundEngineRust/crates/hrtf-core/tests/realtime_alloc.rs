use hrtf_core::{
    BinauralRenderer, ConvolutionMode, DistanceModel, DistanceParams, HrtfGrid, InterpolationMode,
    ObjectEffects, ObjectInput, RenderProfile, RoomPreset, Vec3,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

struct CountingAllocator;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static DEALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNTING.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        COUNTING.with(|enabled| {
            if enabled.get() {
                DEALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        COUNTING.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocator_operations_during(run: impl FnOnce()) -> (usize, usize) {
    ALLOCATIONS.with(|count| count.set(0));
    DEALLOCATIONS.with(|count| count.set(0));
    COUNTING.with(|enabled| enabled.set(true));
    run();
    COUNTING.with(|enabled| enabled.set(false));
    (ALLOCATIONS.with(Cell::get), DEALLOCATIONS.with(Cell::get))
}

#[test]
fn process_is_allocation_free_after_prepare() {
    const OBJECTS: usize = 8;
    const FRAMES: usize = 128;
    const HRIR_LENGTH: usize = 32;

    let mut left_hrirs = vec![0.0; HRIR_LENGTH];
    let mut right_hrirs = vec![0.0; HRIR_LENGTH];
    left_hrirs[0] = 1.0;
    right_hrirs[1] = 1.0;
    let grid = HrtfGrid::new(
        48_000,
        vec![0.0],
        vec![0.0],
        HRIR_LENGTH,
        left_hrirs,
        right_hrirs,
    )
    .unwrap();
    let mut renderer = BinauralRenderer::new(
        grid,
        RenderProfile::LowLatency,
        DistanceModel::Inverse,
        DistanceParams::default(),
    )
    .unwrap();
    renderer.prepare(OBJECTS, FRAMES).unwrap();

    let inputs = vec![vec![0.25; FRAMES]; OBJECTS];
    let objects: Vec<_> = inputs
        .iter()
        .enumerate()
        .map(|(index, mono)| ObjectInput {
            slot: index,
            mono,
            azimuth_deg: index as f32 * 10.0,
            elevation_deg: 0.0,
            distance: 1.0 + index as f32,
            gain: 1.0,
        })
        .collect();
    let mut output_left = vec![0.0; FRAMES];
    let mut output_right = vec![0.0; FRAMES];
    renderer
        .process(&objects, &mut output_left, &mut output_right, FRAMES)
        .unwrap();

    let (allocations, deallocations) = allocator_operations_during(|| {
        for _ in 0..16 {
            renderer
                .process(&objects, &mut output_left, &mut output_right, FRAMES)
                .unwrap();
        }
    });

    assert_eq!(
        (allocations, deallocations),
        (0, 0),
        "renderer process performed {allocations} allocations and {deallocations} deallocations"
    );
}

#[test]
fn planar_process_is_allocation_free_after_prepare() {
    const OBJECTS: usize = 8;
    const FRAMES: usize = 128;
    const HRIR_LENGTH: usize = 32;

    let mut left_hrirs = vec![0.0; HRIR_LENGTH];
    let mut right_hrirs = vec![0.0; HRIR_LENGTH];
    left_hrirs[0] = 1.0;
    right_hrirs[1] = 1.0;
    let grid = HrtfGrid::new(
        48_000,
        vec![0.0],
        vec![0.0],
        HRIR_LENGTH,
        left_hrirs,
        right_hrirs,
    )
    .unwrap();
    let mut renderer = BinauralRenderer::new(
        grid,
        RenderProfile::LowLatency,
        DistanceModel::Inverse,
        DistanceParams::default(),
    )
    .unwrap();
    renderer.prepare(OBJECTS, FRAMES).unwrap();
    let input = vec![0.25; OBJECTS * FRAMES];
    let mut params = vec![0.0; OBJECTS * 4];
    for object in 0..OBJECTS {
        params[object * 4] = object as f32 * 10.0;
        params[object * 4 + 2] = 1.0 + object as f32;
        params[object * 4 + 3] = 1.0;
    }
    let slots: Vec<u32> = (0..OBJECTS as u32).collect();
    let mut output_left = vec![0.0; FRAMES];
    let mut output_right = vec![0.0; FRAMES];
    renderer
        .process_planar(
            &input,
            FRAMES,
            &slots,
            &params,
            OBJECTS,
            &mut output_left,
            &mut output_right,
            FRAMES,
        )
        .unwrap();

    let operations = allocator_operations_during(|| {
        renderer
            .process_planar(
                &input,
                FRAMES,
                &slots,
                &params,
                OBJECTS,
                &mut output_left,
                &mut output_right,
                FRAMES,
            )
            .unwrap();
    });
    assert_eq!(operations, (0, 0));
}

#[test]
fn partitioned_process_is_allocation_free_after_prepare() {
    const OBJECTS: usize = 64;
    const FRAMES: usize = 128;
    const HRIR_LENGTH: usize = 257;
    let mut left_hrirs = vec![0.0; HRIR_LENGTH];
    let mut right_hrirs = vec![0.0; HRIR_LENGTH];
    for tap in 0..HRIR_LENGTH {
        left_hrirs[tap] = 0.97_f32.powi(tap as i32) * 0.1;
        right_hrirs[tap] = 0.96_f32.powi(tap as i32) * 0.1;
    }
    let grid = HrtfGrid::new(
        48_000,
        vec![0.0],
        vec![0.0],
        HRIR_LENGTH,
        left_hrirs,
        right_hrirs,
    )
    .unwrap();
    let mut renderer = BinauralRenderer::new(
        grid,
        RenderProfile::LowLatency,
        DistanceModel::Inverse,
        DistanceParams::default(),
    )
    .unwrap();
    renderer.prepare(OBJECTS, FRAMES).unwrap();
    renderer
        .set_convolution_mode(ConvolutionMode::Partitioned)
        .unwrap();
    let inputs = vec![vec![0.25; FRAMES]; OBJECTS];
    let objects: Vec<_> = inputs
        .iter()
        .enumerate()
        .map(|(slot, mono)| ObjectInput {
            slot,
            mono,
            azimuth_deg: 0.0,
            elevation_deg: 0.0,
            distance: 1.0,
            gain: 1.0,
        })
        .collect();
    let mut left = vec![0.0; FRAMES];
    let mut right = vec![0.0; FRAMES];
    renderer
        .process(&objects, &mut left, &mut right, FRAMES)
        .unwrap();

    let operations = allocator_operations_during(|| {
        renderer
            .process(&objects, &mut left, &mut right, FRAMES)
            .unwrap();
    });
    assert_eq!(operations, (0, 0));
}

#[test]
fn spherical_room_process_is_allocation_free_after_prepare() {
    const OBJECTS: usize = 8;
    const FRAMES: usize = 128;
    const HRIR_LENGTH: usize = 16;
    let azimuths = vec![-157.5_f32, -112.5, -67.5, -22.5, 22.5, 67.5, 112.5, 157.5];
    let elevations = vec![-70.0_f32, -35.0, -10.0, 15.0, 45.0, 75.0];
    let samples = azimuths.len() * elevations.len() * HRIR_LENGTH;
    let mut left_hrirs = vec![0.0; samples];
    let mut right_hrirs = vec![0.0; samples];
    for direction in 0..azimuths.len() * elevations.len() {
        left_hrirs[direction * HRIR_LENGTH] = 0.5;
        right_hrirs[direction * HRIR_LENGTH + 1] = 0.5;
    }
    let grid = HrtfGrid::new(
        48_000,
        azimuths,
        elevations,
        HRIR_LENGTH,
        left_hrirs,
        right_hrirs,
    )
    .unwrap();
    let mut renderer = BinauralRenderer::new(
        grid,
        RenderProfile::LowLatency,
        DistanceModel::Inverse,
        DistanceParams::default(),
    )
    .unwrap();
    renderer
        .set_interpolation_mode(InterpolationMode::Spherical)
        .unwrap();
    renderer.set_room_preset(Some(RoomPreset::Studio)).unwrap();
    renderer.set_room_amount(0.4).unwrap();
    renderer.prepare(OBJECTS, FRAMES).unwrap();
    let inputs = vec![vec![0.25; FRAMES]; OBJECTS];
    let objects: Vec<_> = inputs
        .iter()
        .enumerate()
        .map(|(index, mono)| ObjectInput {
            slot: index,
            mono,
            azimuth_deg: index as f32 * 10.0,
            elevation_deg: 0.0,
            distance: 1.0 + index as f32,
            gain: 1.0,
        })
        .collect();
    let mut output_left = vec![0.0; FRAMES];
    let mut output_right = vec![0.0; FRAMES];
    renderer
        .process(&objects, &mut output_left, &mut output_right, FRAMES)
        .unwrap();

    let operations = allocator_operations_during(|| {
        for _ in 0..16 {
            renderer
                .process(&objects, &mut output_left, &mut output_right, FRAMES)
                .unwrap();
        }
    });
    assert_eq!(operations, (0, 0));
}

#[test]
fn object_effects_process_is_allocation_free_after_prepare() {
    const OBJECTS: usize = 8;
    const FRAMES: usize = 128;
    let mut left_hrirs = vec![0.0; 32];
    let mut right_hrirs = vec![0.0; 32];
    left_hrirs[0] = 1.0;
    right_hrirs[1] = 1.0;
    let grid = HrtfGrid::new(48_000, vec![0.0], vec![0.0], 32, left_hrirs, right_hrirs).unwrap();
    let mut renderer = BinauralRenderer::new(
        grid,
        RenderProfile::LowLatency,
        DistanceModel::Inverse,
        DistanceParams::default(),
    )
    .unwrap();
    renderer.prepare(OBJECTS * 2, FRAMES).unwrap();
    renderer.set_occlusion(0.5).unwrap();
    renderer
        .set_listener_velocity(Some(Vec3 {
            x: 0.0,
            y: 0.0,
            z: 8.0,
        }))
        .unwrap();
    let inputs = vec![vec![0.25; FRAMES]; OBJECTS];
    let objects: Vec<_> = inputs
        .iter()
        .enumerate()
        .map(|(slot, mono)| ObjectInput {
            slot,
            mono,
            azimuth_deg: slot as f32 * 10.0,
            elevation_deg: 0.0,
            distance: 1.0 + slot as f32,
            gain: 1.0,
        })
        .collect();
    let effects: Vec<_> = (0..OBJECTS)
        .map(|slot| ObjectEffects {
            size: 0.5,
            spread_slot: Some(slot + OBJECTS),
        })
        .collect();
    let mut left = vec![0.0; FRAMES];
    let mut right = vec![0.0; FRAMES];
    renderer
        .process_with_effects(&objects, &effects, &mut left, &mut right, FRAMES)
        .unwrap();

    let operations = allocator_operations_during(|| {
        for _ in 0..16 {
            renderer
                .process_with_effects(&objects, &effects, &mut left, &mut right, FRAMES)
                .unwrap();
        }
    });
    assert_eq!(operations, (0, 0));
}
