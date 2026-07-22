using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Integrations;
using Mvf.Sdk;

namespace MachineVisionFabric.Integrations.FolderSource;

/// <summary>
/// A hardware-free simulator source: emits the files of a folder as frames, in name order. Lets a whole
/// pipeline run and be tested without a real camera (the Simulator-First principle).
/// </summary>
public sealed class FolderSourceModule : FrameSourceModuleBase<FolderSourceOptions>
{
    protected override IntegrationModuleDescriptor BuildDescriptor() =>
        IntegrationModuleDescriptorBuilder.CreateSource<FolderSourceOptions>(
            moduleId:       "mvf.folder-source",
            displayName:    "Folder Sequence Source",
            version:        "1.0.0",
            capabilityName: "folder-sequence-source",
            description:    "Emits the files of a folder as frames, in name order — a hardware-free simulator source.");

    protected override IFrameSourceSession OpenSession(FolderSourceOptions options, string packageRoot)
    {
        var folder = Path.IsPathRooted(options.SourceFolder)
            ? options.SourceFolder
            : Path.Combine(packageRoot, options.SourceFolder);

        var files = Directory.Exists(folder)
            ? Directory.EnumerateFiles(folder).OrderBy(path => path, StringComparer.Ordinal).ToList()
            : [];

        return new FileSequenceFrameSourceSession(files, declaredCameraCount: 1, options.FrameIntervalMs, options.Loop);
    }
}
