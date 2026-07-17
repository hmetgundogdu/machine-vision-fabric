using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Integrations.DatasetWriter;

/// <summary>
/// Integration module that writes accepted frames as image + JSON metadata files
/// under a timestamped session directory.
///
/// This is a sink module — it has one <c>frame</c> input port and no outputs.
/// </summary>
public sealed class DatasetWriterIntegrationModule : FrameSinkModuleBase<DatasetWriterOptions>
{
    public const string ModuleId = "mvf.dataset-writer";

    protected override IntegrationModuleDescriptor BuildDescriptor()
    {
        return IntegrationModuleDescriptorBuilder.CreateSink<DatasetWriterOptions>(
            moduleId: ModuleId,
            displayName: "Dataset Writer",
            version: "1.0.0",
            capabilityName: "dataset-writer",
            description: "Writes each accepted frame as an image file with JSON metadata into a timestamped session directory.");
    }

    protected override IFrameSink OpenSink(DatasetWriterOptions options, string packageRoot)
    {
        var root = Path.IsPathRooted(options.OutputRoot)
            ? options.OutputRoot
            : Path.Combine(packageRoot, options.OutputRoot);

        var timestamp = DateTime.Now;
        var sessionName = $"{options.SessionPrefix}-{timestamp:yyyyMMdd}-{timestamp:HHmmss}-{timestamp.Millisecond:000}";
        var sessionRoot = Path.Combine(root, sessionName);

        return new DatasetWriterSink(sessionRoot, options.IncludeSourcePathInMetadata);
    }
}
