using MachineVisionFabric.Contracts.Simulation;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Core.Frames;

namespace MachineVisionFabric.Sources.Simulators;

public sealed class FolderSequenceSourceCatalog : ISimulatorSourceCatalog
{
    private static readonly string[] SupportedExtensions = [".jpg", ".jpeg", ".png", ".bmp", ".tif", ".tiff"];

    public IFrameSourceSession OpenSession(FolderSequenceSourceOptions options)
    {
        var repositoryRoot = ResolveRepositoryRoot(typeof(FolderSequenceSourceCatalog).Assembly.Location);

        var sourceFolder = Path.IsPathRooted(options.SourceFolder)
            ? Path.GetFullPath(options.SourceFolder)
            : Path.GetFullPath(Path.Combine(repositoryRoot, options.SourceFolder));
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

    private static string ResolveRepositoryRoot(string assemblyLocation)
    {
        var currentDirectory = new DirectoryInfo(Path.GetDirectoryName(assemblyLocation) ?? AppContext.BaseDirectory);
        while (currentDirectory is not null)
        {
            var hasSrc = Directory.Exists(Path.Combine(currentDirectory.FullName, "src"));
            var hasExamples = Directory.Exists(Path.Combine(currentDirectory.FullName, "examples"));
            var hasLegacySamples = Directory.Exists(Path.Combine(currentDirectory.FullName, "samples"));
            if (hasSrc && (hasExamples || hasLegacySamples))
            {
                return currentDirectory.FullName;
            }

            currentDirectory = currentDirectory.Parent;
        }

        return Path.GetDirectoryName(assemblyLocation) ?? AppContext.BaseDirectory;
    }
}
