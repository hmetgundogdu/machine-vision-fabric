using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Contracts.Simulation;
using MachineVisionFabric.Core.Abstractions;
using Microsoft.Extensions.Logging;

namespace MachineVisionFabric.Runtime;

public sealed class ProfileFrameSourceResolver(
    ISimulatorSourceCatalog simulatorSourceCatalog,
    IIntegrationModuleLoader integrationModuleLoader,
    ILogger<ProfileFrameSourceResolver> logger) : IFrameSourceResolver
{
    public FrameSourceResolution Resolve(FabricRuntimeProfile profile, string packageRoot, string integrationsRoot)
    {
        var binding = profile.Source;
        if (string.Equals(binding.Mode, "module", StringComparison.OrdinalIgnoreCase))
        {
            var modules = integrationModuleLoader.LoadModules(integrationsRoot)
                .OfType<IFrameSourceModule>()
                .ToArray();

            var selectedModule = modules.FirstOrDefault(module =>
                string.Equals(module.Describe().ModuleId, binding.ModuleId, StringComparison.OrdinalIgnoreCase));

            if (selectedModule is null)
            {
                throw new InvalidOperationException(
                    $"Frame source module '{binding.ModuleId}' could not be found under '{integrationsRoot}'.");
            }

            var moduleSession = selectedModule.OpenSession(binding.Config, packageRoot);

            logger.LogInformation(
                "Resolved frame source through external module {ModuleId}.",
                selectedModule.Describe().ModuleId);

            return new FrameSourceResolution(
                moduleSession,
                "module",
                selectedModule.Describe().ModuleId);
        }

        var simulatorSource = ResolveSimulatorSource(profile, packageRoot);
        var builtinSession = simulatorSourceCatalog.OpenSession(simulatorSource);

        logger.LogInformation("Resolved frame source through built-in folder sequence simulator.");

        return new FrameSourceResolution(
            builtinSession,
            "builtin",
            "builtin-folder-sequence");
    }

    private static FolderSequenceSourceOptions ResolveSimulatorSource(FabricRuntimeProfile profile, string packageRoot)
    {
        var source = profile.SimulatorSource;

        return new FolderSequenceSourceOptions
        {
            SourceFolder = Path.IsPathRooted(source.SourceFolder)
                ? Path.GetFullPath(source.SourceFolder)
                : Path.GetFullPath(Path.Combine(packageRoot, source.SourceFolder)),
            Loop = source.Loop,
            FrameIntervalMs = source.FrameIntervalMs,
            ParallelCameraCount = source.ParallelCameraCount
        };
    }
}
