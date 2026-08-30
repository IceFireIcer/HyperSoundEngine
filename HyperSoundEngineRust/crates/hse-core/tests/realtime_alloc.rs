use hse_core::{
    convolver::{ConvolverOptions, ConvolverStage},
    engine_chain::{EngineChainParams, EngineChainStage},
    Stage,
};
use serde_json::json;
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
fn convolver_process_is_allocation_free_after_prepare() {
    let frames = 4096;
    let mut stage = ConvolverStage::new(
        48_000.0,
        ConvolverOptions {
            partition_size: 32.0,
            long_partition_size: 32.0,
            short_region_ms: 0.0,
            de_periodize: false,
        },
    )
    .unwrap();
    stage.load_ir(&[1.0], None).unwrap();
    stage.prepare(frames);
    let mut left = vec![0.25; frames];
    let mut right = vec![-0.25; frames];

    let (allocations, deallocations) =
        allocator_operations_during(|| stage.process(&mut left, &mut right));

    assert_eq!(
        (allocations, deallocations),
        (0, 0),
        "convolver process performed {allocations} allocations and {deallocations} deallocations"
    );
}

#[test]
fn default_engine_chain_is_allocation_free_after_prepare() {
    assert_chain_process_is_allocation_free(json!({}));
}

#[test]
fn representative_all_enabled_engine_chain_is_allocation_free_after_prepare() {
    assert_chain_process_is_allocation_free(json!({
        "loudnessNormalization": {"enabled": true},
        "surround3d": {"enabled": true},
        "deesser": {"enabled": true},
        "compressor": {"enabled": true},
        "nightMode": {"enabled": true, "amount": 0.8},
        "modEffects": {
            "delay": {"enabled": true},
            "chorus": {"enabled": true},
            "flanger": {"enabled": true},
            "phaser": {"enabled": true},
            "tremolo": {"enabled": true}
        },
        "reverb": {
            "enabled": true,
            "mode": "convolution",
            "convolution": {"ir": [1.0], "dePeriodize": false}
        },
        "bassEnhancer": {"enabled": true},
        "loudnessCompensation": {"enabled": true},
        "ieq": {"enabled": true},
        "dynamicEq": {"enabled": true},
        "pitch": {"enabled": true, "voiceBalance": 0.25},
        "modulation": {
            "enabled": true,
            "routes": [
                {"source": "lfo", "target": "masterGain", "amount": 0.2},
                {"source": "envelope", "target": "stereoWidth", "amount": 0.2}
            ]
        }
    }));
}

fn assert_chain_process_is_allocation_free(overrides: serde_json::Value) {
    let frames = 4096;
    let params = EngineChainParams::from_overrides(48_000.0, &overrides).unwrap();
    let mut stage = EngineChainStage::from_params(48_000.0, params).unwrap();
    stage.prepare(frames);
    let mut left = vec![0.125; frames];
    let mut right = vec![-0.125; frames];

    let (allocations, deallocations) =
        allocator_operations_during(|| stage.process(&mut left, &mut right));

    assert_eq!(
        (allocations, deallocations),
        (0, 0),
        "engine chain process performed {allocations} allocations and {deallocations} deallocations"
    );
}
