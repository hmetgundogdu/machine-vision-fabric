using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Integrations.DatasetWriter;

/// <summary>
/// Sink module that writes each incoming frame as an image file with JSON
/// metadata under a timestamped session directory.
///
/// Node category: output — one <c>frame</c> input port, no outputs.
/// Instantiate it more than once (different <c>outputRoot</c>) to fan captures
/// into separate datasets — e.g. an "all frames" writer and a "dark frames" writer.
/// </summary>
public sealed class DatasetWriterIntegrationModule : FrameSinkModuleBase<DatasetWriterOptions>
{
    public const string ModuleId = "mvf.dataset-writer";

    protected override IntegrationModuleDescriptor BuildDescriptor() =>
        IntegrationModuleDescriptorBuilder.CreateSink<DatasetWriterOptions>(
            moduleId:       ModuleId,
            displayName:    "Dataset Writer",
            version:        "1.0.0",
            capabilityName: "dataset-writer",
            description:    "Writes each incoming frame as an image file with JSON metadata into a timestamped session directory.");

    protected override IFrameSink OpenSink(DatasetWriterOptions options, string packageRoot)
    {
        var root = Path.IsPathRooted(options.OutputRoot)
            ? options.OutputRoot
            : Path.Combine(packageRoot, options.OutputRoot);

        var timestamp   = DateTime.Now;
        var sessionName = $"{options.SessionPrefix}-{timestamp:yyyyMMdd}-{timestamp:HHmmss}-{timestamp.Millisecond:000}";
        var sessionRoot = Path.Combine(root, sessionName);

        return new DatasetWriterSink(sessionRoot, options.IncludeSourcePathInMetadata);
    }
}
