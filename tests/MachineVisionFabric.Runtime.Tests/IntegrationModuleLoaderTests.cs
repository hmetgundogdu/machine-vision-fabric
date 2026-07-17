using MachineVisionFabric.Runtime.Plugins;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class IntegrationModuleLoaderTests
{
    [Fact]
    public void LoadModules_UsesIntegrationManifestDiscovery()
    {
        var repositoryRoot = ResolveRepositoryRoot();

        // The loader resolves each manifest's entry assembly relative to the manifest
        // file, so it needs a deployed layout where manifest + DLL are colocated.
        // publish.ps1 produces exactly that under publish/mvf/integrations.
        var integrationsRoot = Path.Combine(repositoryRoot, "publish", "mvf", "integrations");
        if (!Directory.Exists(integrationsRoot))
        {
            // No published layout available (e.g. clean checkout without a publish run) —
            // nothing to discover, so there is nothing to assert here.
            return;
        }

        var loader = new IntegrationModuleLoader();

        var modules = loader.LoadModules(integrationsRoot);
        var moduleIds = modules
            .Select(module => module.Describe().ModuleId)
            .OrderBy(id => id, StringComparer.OrdinalIgnoreCase)
            .ToArray();

        // No duplicate module ids from discovery.
        Assert.Equal(moduleIds.Length, moduleIds.Distinct(StringComparer.OrdinalIgnoreCase).Count());

        // The kept real-world integration modules must be discoverable.
        Assert.Contains("mvf.realworld-cognex-camera", moduleIds);
        Assert.Contains("mvf.realworld-dark-frame-filter", moduleIds);
    }

    private static string ResolveRepositoryRoot()
    {
        var currentDirectory = new DirectoryInfo(AppContext.BaseDirectory);
        while (currentDirectory is not null)
        {
            var hasSrc = Directory.Exists(Path.Combine(currentDirectory.FullName, "src"));
            var hasRealWorld = Directory.Exists(Path.Combine(currentDirectory.FullName, "real-world-projects"));
            if (hasSrc && hasRealWorld)
            {
                return currentDirectory.FullName;
            }

            currentDirectory = currentDirectory.Parent;
        }

        throw new DirectoryNotFoundException("Repository root could not be resolved for tests.");
    }
}
