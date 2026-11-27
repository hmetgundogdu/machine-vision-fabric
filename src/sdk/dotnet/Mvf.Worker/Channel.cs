using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace Mvf.Worker;

/// <summary>
/// Moves the protocol off the standard descriptors (protocol/worker-v2.md, "Protect the channel"):
/// the SDK keeps private duplicates of stdin and stdout for the protocol, then points stdout at
/// stderr and stdin at the null device, for managed code (Console) and native code alike. A stray
/// Console.WriteLine or a native library writing to stdout then lands in the logs.
/// </summary>
internal static unsafe partial class Channel
{
    public static (Stream Input, Stream Output) Protect()
    {
        var (input, output) = OperatingSystem.IsWindows() ? ProtectWindows() : ProtectUnix();
        Console.SetOut(new StreamWriter(Console.OpenStandardError(), new UTF8Encoding(false))
        {
            AutoFlush = true,
        });
        Console.SetIn(TextReader.Null);
        return (input, output);
    }

    private static (Stream, Stream) ProtectUnix()
    {
        // libc through the main program's global scope: glibc, musl and macOS alike.
        var libc = NativeLibrary.GetMainProgramHandle();
        var dup = (delegate* unmanaged<int, int>)NativeLibrary.GetExport(libc, "dup");
        var dup2 = (delegate* unmanaged<int, int, int>)NativeLibrary.GetExport(libc, "dup2");
        var open = (delegate* unmanaged<byte*, int, int>)NativeLibrary.GetExport(libc, "open");
        var close = (delegate* unmanaged<int, int>)NativeLibrary.GetExport(libc, "close");
        int inFd = dup(0), outFd = dup(1);
        if (inFd < 0 || outFd < 0)
        {
            throw new IOException("protocol v2: cannot duplicate stdio");
        }

        // A module's own child processes must not inherit the protocol descriptors. fcntl is
        // variadic, and Apple arm64 passes variadic arguments on the stack, so it cannot be called
        // through a fixed signature there; that one platform keeps them inheritable.
        if (!(OperatingSystem.IsMacOS() && RuntimeInformation.ProcessArchitecture == Architecture.Arm64))
        {
            var fcntl = (delegate* unmanaged<int, int, int, int>)NativeLibrary.GetExport(libc, "fcntl");
            const int F_SETFD = 2, FD_CLOEXEC = 1;
            fcntl(inFd, F_SETFD, FD_CLOEXEC);
            fcntl(outFd, F_SETFD, FD_CLOEXEC);
        }

        fixed (byte* path = "/dev/null\0"u8)
        {
            var nul = open(path, 0); // O_RDONLY
            if (nul >= 0)
            {
                dup2(nul, 0);
                close(nul);
            }
        }

        dup2(2, 1);
        return (Stream(inFd, FileAccess.Read), Stream(outFd, FileAccess.Write));
    }

    private static (Stream, Stream) ProtectWindows()
    {
        const int StdInput = -10, StdOutput = -11, StdError = -12;
        var self = GetCurrentProcess();
        if (!DuplicateHandle(self, GetStdHandle(StdInput), self, out var input, 0, false, 2) ||
            !DuplicateHandle(self, GetStdHandle(StdOutput), self, out var output, 0, false, 2))
        {
            throw new IOException("protocol v2: cannot duplicate stdio");
        }

        var nul = CreateFileW("NUL", 0x80000000, 3, 0, 3, 0, 0); // GENERIC_READ, share r/w, OPEN_EXISTING
        SetStdHandle(StdInput, nul);
        SetStdHandle(StdOutput, GetStdHandle(StdError));
        // Native code that writes through the C runtime (printf) uses its descriptor table, which
        // was filled at startup: point fd 1 at fd 2 and fd 0 at NUL there too.
        if (NativeLibrary.TryLoad("ucrtbase.dll", out var ucrt) &&
            NativeLibrary.TryGetExport(ucrt, "_dup2", out var dup2Fn) &&
            NativeLibrary.TryGetExport(ucrt, "_open_osfhandle", out var openFn))
        {
            var dup2 = (delegate* unmanaged<int, int, int>)dup2Fn;
            var openOsf = (delegate* unmanaged<nint, int, int>)openFn;
            dup2(2, 1);
            var crtNul = CreateFileW("NUL", 0x80000000, 3, 0, 3, 0, 0);
            var fd = openOsf(crtNul, 0);
            if (fd >= 0)
            {
                dup2(fd, 0);
            }
        }

        return (new FileStream(new SafeFileHandle(input, true), FileAccess.Read, 0),
            new FileStream(new SafeFileHandle(output, true), FileAccess.Write, 0));
    }

    private static FileStream Stream(int fd, FileAccess access) =>
        new(new SafeFileHandle(fd, ownsHandle: true), access, bufferSize: 0);

    [LibraryImport("kernel32.dll")]
    private static partial nint GetStdHandle(int which);

    [LibraryImport("kernel32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetStdHandle(int which, nint handle);

    [LibraryImport("kernel32.dll")]
    private static partial nint GetCurrentProcess();

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool DuplicateHandle(nint sourceProcess, nint source, nint targetProcess,
        out nint target, uint access, [MarshalAs(UnmanagedType.Bool)] bool inherit, uint options);

    [LibraryImport("kernel32.dll", StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    private static partial nint CreateFileW(string name, uint access, uint share, nint security,
        uint disposition, uint flags, nint template);
}
