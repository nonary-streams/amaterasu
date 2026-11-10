//! A separately built, CPU BGRA8 pass-through processor. This example forwards
//! the source owner's retained reference; it does not change pixel values.
use amaterasu_media::*;
use amaterasu_module_sdk::{
    ManagedModule, ModuleResult, RuntimeClient,
    abi::{STATUS_INVALID_ARGUMENT, Status},
    frame_processing::{
        FrameProcessor, FrameProcessorFactory, FrameResourceSelection, ProcessingFrame,
        ResourceOutputFrame,
    },
    media::native_contracts,
    processing_composition,
};
use std::{collections::BTreeMap, ptr};

const SERVICE: &str = "example.frame-processor/1";
const GENERATION: u64 = 1;
const OFFER: u64 = 1;

#[derive(Default)]
struct Stream {
    input: bool,
    output: bool,
}

struct Passthrough {
    capabilities: Box<CpuResourceCapabilitiesV2>,
    streams: BTreeMap<u64, Stream>,
}

impl Passthrough {
    fn new() -> Self {
        Self {
            capabilities: Box::new(CpuResourceCapabilitiesV2 {
                base: CpuResourceCapabilities {
                    format: PIXEL_BGRA8,
                    min_width: 16,
                    max_width: 8192,
                    min_height: 16,
                    max_height: 8192,
                },
                color: FrameColorMetadata {
                    version: 1,
                    size: std::mem::size_of::<FrameColorMetadata>() as u32,
                    primaries: FRAME_PRIMARIES_BT709,
                    transfer: FRAME_TRANSFER_SRGB,
                    matrix: FRAME_MATRIX_RGB,
                    range: FRAME_RANGE_FULL,
                },
            }),
            streams: BTreeMap::new(),
        }
    }

    fn offer(&self) -> CaptureResourceOffer {
        let none = FrameDescriptor {
            kind: FRAME_SYNC_NONE,
            version: 0,
            size: 0,
            reserved: 0,
            namespace: [0; 16],
            data: ptr::null(),
        };
        CaptureResourceOffer {
            id: OFFER,
            ownership: FRAME_OWNERSHIP_RETAINABLE,
            accessibility: FRAME_ACCESS_CPU,
            reserved: 0,
            resource: FrameDescriptor {
                kind: FRAME_RESOURCE_CPU,
                version: 2,
                size: std::mem::size_of::<CpuResourceCapabilitiesV2>() as u32,
                reserved: 0,
                namespace: [0; 16],
                data: ptr::from_ref(&*self.capabilities).cast(),
            },
            acquire_sync: none,
            completion_sync: none,
        }
    }
}

impl FrameProcessor for Passthrough {
    fn configure_stream(&mut self, stream: u64, settings: &[u8]) -> Result<(), Status> {
        if serde_json::from_slice::<serde_json::Value>(settings)
            .ok()
            .and_then(|value| value.as_object().map(|object| object.is_empty()))
            != Some(true)
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        self.streams.insert(stream, Stream::default());
        Ok(())
    }
    fn update_stream(&mut self, _stream: u64, _settings: &[u8]) -> Result<(), Status> {
        Err(MEDIA_UNSUPPORTED)
    }
    fn input_offers(&mut self, _stream: u64) -> Result<(u64, Vec<CaptureResourceOffer>), Status> {
        Ok((GENERATION, vec![self.offer()]))
    }
    fn configure_input(
        &mut self,
        stream: u64,
        generation: u64,
        offer: u64,
        _source_generation: u64,
        source: &CaptureResourceOffer,
    ) -> Result<(), Status> {
        let descriptor = &source.resource;
        if (generation, offer) != (GENERATION, OFFER)
            || source.ownership != FRAME_OWNERSHIP_RETAINABLE
            || descriptor.kind != FRAME_RESOURCE_CPU
            || descriptor.version != 2
            || descriptor.namespace != [0; 16]
            || descriptor.reserved != 0
            || descriptor.size < std::mem::size_of::<CpuResourceCapabilitiesV2>() as u32
            || descriptor.data.is_null()
            || !(descriptor.data as usize)
                .is_multiple_of(std::mem::align_of::<CpuResourceCapabilitiesV2>())
            || source.acquire_sync.kind != FRAME_SYNC_NONE
            || source.completion_sync.kind != FRAME_SYNC_NONE
        {
            return Err(MEDIA_UNSUPPORTED);
        }
        // SAFETY: the native SDK contract lends the complete, aligned v2 offer
        // payload for the duration of this callback; no pointer is retained.
        let caps = unsafe { &*descriptor.data.cast::<CpuResourceCapabilitiesV2>() };
        let color = &caps.color;
        if caps.base.format != PIXEL_BGRA8
            || caps.base.min_width > self.capabilities.base.max_width
            || caps.base.max_width < self.capabilities.base.min_width
            || caps.base.min_height > self.capabilities.base.max_height
            || caps.base.max_height < self.capabilities.base.min_height
            || color.version != 1
            || color.size < std::mem::size_of::<FrameColorMetadata>() as u32
            || color.primaries != FRAME_PRIMARIES_BT709
            || color.transfer != FRAME_TRANSFER_SRGB
            || color.matrix != FRAME_MATRIX_RGB
            || color.range != FRAME_RANGE_FULL
        {
            return Err(MEDIA_UNSUPPORTED);
        }
        self.streams
            .get_mut(&stream)
            .ok_or(STATUS_INVALID_ARGUMENT)?
            .input = true;
        Ok(())
    }
    fn output_offers(&mut self, stream: u64) -> Result<(u64, Vec<CaptureResourceOffer>), Status> {
        if !self.streams.get(&stream).is_some_and(|s| s.input) {
            return Err(MEDIA_UNSUPPORTED);
        }
        Ok((GENERATION, vec![self.offer()]))
    }
    fn select_output(&mut self, stream: u64, generation: u64, offer: u64) -> Result<(), Status> {
        if (generation, offer) != (GENERATION, OFFER) {
            return Err(MEDIA_UNSUPPORTED);
        }
        self.streams
            .get_mut(&stream)
            .ok_or(STATUS_INVALID_ARGUMENT)?
            .output = true;
        Ok(())
    }
    fn process(
        &mut self,
        stream: u64,
        input: ProcessingFrame<'_>,
    ) -> Result<ResourceOutputFrame, Status> {
        if !self
            .streams
            .get(&stream)
            .is_some_and(|s| s.input && s.output)
        {
            return Err(MEDIA_UNSUPPORTED);
        }
        input.forward(FrameResourceSelection {
            generation: GENERATION,
            offer: OFFER,
        })
    }
    fn hdr_metadata(&self, _stream: u64) -> Option<HdrMetadata> {
        None
    }
    fn cancel(&mut self, stream: u64) {
        self.streams.remove(&stream);
    }
    fn shutdown(&mut self) {
        self.streams.clear();
    }
}

type CreateProcessor = fn(&str) -> Result<Passthrough, Status>;
struct Example {
    _factory: Box<FrameProcessorFactory<CreateProcessor>>,
    runtime: RuntimeClient,
}
impl ManagedModule for Example {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let composition = runtime
            .processing_composition()?
            .ok_or("frame processor not selected")?;
        let selected = composition
            .processor
            .as_ref()
            .ok_or("processor binding missing")?;
        processing_composition::require_contract(
            selected,
            native_contracts::FRAME_PROCESSOR,
            1,
            0,
        )?;
        if selected.service != SERVICE {
            return Err("processor binding differs from this provider".into());
        }
        let create: CreateProcessor = |_| Ok(Passthrough::new());
        let factory = FrameProcessorFactory::new(create);
        runtime.provide(SERVICE)?;
        // SAFETY: the factory remains owned by the module until all consumers retire.
        unsafe { factory.register(&runtime, SERVICE) }?;
        Ok(Self {
            _factory: factory,
            runtime,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(request) = self.runtime.next_request()? {
            request.reply(
                &self.runtime,
                &serde_json::json!({"ok":false,"code":"unsupported"}),
            )?;
        }
        Ok(())
    }
}
amaterasu_module_sdk::export_module!(Example);
