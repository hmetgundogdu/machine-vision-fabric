namespace MachineVisionFabric.Core.Abstractions;

public sealed record FrameProcessorResolution(
    IFrameProcessor? Processor,
    string Source,
    string Strategy);
