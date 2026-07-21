namespace Mvf.Graph.Simulation;

public sealed record SimulatorFrameDescriptor(
    string CameraId,
    int SequenceNumber,
    string SourcePath,
    string FileName);
