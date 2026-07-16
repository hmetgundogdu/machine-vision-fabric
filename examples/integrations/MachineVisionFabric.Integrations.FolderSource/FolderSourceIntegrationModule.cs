using System.Text.Json;
using MachineVisionFabric.Contracts.Simulation;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Core.Frames;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Integrations.FolderSource;

public sealed class FolderSourceIntegrationModule : FrameSourceModuleBase<FolderSequenceSourceOptions>
{
    private static readonly string[] SupportedExtensions = [".jpg", ".jpeg", ".png", ".bmp", ".tif", ".tiff"];

    protected override MachineVisionFabric.Contracts.Integrations.IntegrationModuleDescriptor BuildDescriptor()
    {
        return IntegrationModuleDescriptorBuilder.CreateSource<FolderSequenceSourceOptions>(
            "mvf.folder-source",
            "Folder Sequence Source",
            "0.1.0",
            "folder-sequence-source",
            "Streams image files from a package-relative folder as dataset frames.");
    }

    protected override IFrameSourceSession OpenSession(FolderSequenceSourceOptions options, string packageRoot)
    {
        var sourceFolder = PackagePathResolver.Resolve(packageRoot, options.SourceFolder);

        if (!Directory.Exists(sourceFolder))
        {
            return new FileSequenceFrameSourceSession([], Math.Max(1, options.ParallelCameraCount), options.FrameIntervalMs, options.Loop);
        }

        var files = Directory
            .EnumerateFiles(sourceFolder, "*.*", SearchOption.TopDirectoryOnly)
            .Where(file => SupportedExtensions.Contains(Path.GetExtension(file), StringComparer.OrdinalIgnoreCase))
            .OrderBy(file => file, StringComparer.OrdinalIgnoreCase)
            .ToArray();

        return new FileSequenceFrameSourceSession(
            files,
            Math.Max(1, options.ParallelCameraCount),
            options.FrameIntervalMs,
            options.Loop);
    }
}
