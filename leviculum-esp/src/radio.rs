//! The radio blobs' common ground on the M9: one scheduler and one more
//! internal heap region, shared by BLE (1480) and WiFi (1479).
//!
//! esp-radio's BLE controller and WiFi driver both run preemptive tasks
//! under `esp_rtos`, and both allocate only through `malloc_internal`
//! from esp-alloc's internal regions. The scheduler can start once per
//! boot, so it starts here, before whichever carrier comes first, and
//! not inside either one: a board with BLE switched off still runs WiFi.
//!
//! The extra region is the memory the ESP-IDF second-stage bootloader
//! used and left behind (`dram2_seg` in esp-hal's linker script,
//! 0x3FCDB700..0x3FCED710 on the S3, 73,744 B). Nothing else of the
//! image lives there, so the region costs neither core 0's stack nor the
//! UI core's: `scripts/build-m9-image.sh`'s STACK_HEADROOM lines do not
//! move when it grows. The scheduler's own task stacks, the WiFi
//! driver's buffers and whatever the BLE blobs need past their 8 KiB
//! (`ble::BLE_INTERNAL_HEAP_BYTES`) come from it.

use core::mem::MaybeUninit;

use esp_alloc::{HeapRegion, MemoryCapability};
use esp_hal::peripherals::FROM_CPU_INTR0;
use esp_hal::timer::timg::Timer;

/// The reclaimed bootloader region given to the blobs, all of it but the
/// 16 B the section's alignment may cost.
pub const RECLAIMED_HEAP_BYTES: usize = 72 * 1024;

/// Hand the reclaimed region to esp-alloc and start the blobs'
/// scheduler. Core 0's loop becomes the scheduler's main task and runs
/// on; the UI core keeps its own regime. Called at most once, before
/// `ble::start` and `wifi::start`; the caller calls it only when a
/// carrier is wanted, so a board with every radio off never starts it.
pub fn start(timer: Timer<'static>, int0: FROM_CPU_INTR0<'static>) {
    #[unsafe(link_section = ".dram2_uninit")]
    static mut RECLAIMED: MaybeUninit<[u8; RECLAIMED_HEAP_BYTES]> = MaybeUninit::uninit();
    // SAFETY: the static lives in the reclaimed bootloader segment, which
    // nothing else of the image is linked into and the bootloader no
    // longer uses once the app runs; it is handed to esp-alloc here once
    // (`start` takes the peripherals by value, so a second call cannot
    // compile into the same boot) and never touched otherwise.
    unsafe {
        esp_alloc::HEAP.add_region(HeapRegion::new(
            core::ptr::addr_of_mut!(RECLAIMED).cast(),
            RECLAIMED_HEAP_BYTES,
            MemoryCapability::Internal.into(),
        ));
    }
    esp_rtos::start(timer, int0);
}
