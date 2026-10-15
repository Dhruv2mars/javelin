// Disposable hosted fixture helper. Does not replace or modify the inherited capture implementation.
#define WIN32_LEAN_AND_MEAN
#define WIN32_NO_STATUS
#include <windows.h>
#undef WIN32_NO_STATUS
#include <ntstatus.h>
#include <bcrypt.h>
#include <cfapi.h>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <cstdio>
#include <cwchar>
#include <string>
#include <vector>

static std::wstring quote(const std::wstring& argument) {
    std::wstring out = L"\""; size_t slashes = 0;
    for (wchar_t c : argument) {
        if (c == L'\\') { ++slashes; continue; }
        out.append(slashes * (c == L'"' ? 2 : 1), L'\\'); slashes = 0;
        if (c == L'"') out += L'\\';
        out += c;
    }
    out.append(slashes * 2, L'\\'); return out + L'"';
}
// Limits the complete ordinary-user process tree without changing inherited sparse.cpp.
static int bounded(int argc, wchar_t** argv) {
    DWORD wall = static_cast<DWORD>(std::stoul(argv[2]));
    if (wall < 100 || wall > 200000) return 2;
    HANDLE job = CreateJobObjectW(nullptr, nullptr);
    if (!job) return 2;
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_JOB_TIME;
    limits.BasicLimitInformation.PerJobUserTimeLimit.QuadPart = 120LL * 10000000;
    if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits, sizeof(limits))) { CloseHandle(job); return 2; }
    std::wstring command;
    for (int i = 3; i < argc; ++i) { if (i > 3) command += L' '; command += quote(argv[i]); }
    STARTUPINFOW startup{}; startup.cb = sizeof(startup); startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE); startup.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE); startup.hStdError = GetStdHandle(STD_ERROR_HANDLE);
    PROCESS_INFORMATION child{};
    if (!CreateProcessW(nullptr, command.data(), nullptr, nullptr, TRUE, CREATE_SUSPENDED, nullptr, nullptr, &startup, &child)) { CloseHandle(job); return 2; }
    if (!AssignProcessToJobObject(job, child.hProcess)) {
        TerminateProcess(child.hProcess, 2); CloseHandle(child.hThread); CloseHandle(child.hProcess); CloseHandle(job); return 2;
    }
    ResumeThread(child.hThread); CloseHandle(child.hThread);
    bool timedOut = WaitForSingleObject(child.hProcess, wall) != WAIT_OBJECT_0;
    DWORD code = 124; if (!timedOut) GetExitCodeProcess(child.hProcess, &code);
    TerminateJobObject(job, 124);
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION accounting{}; bool queried = false;
    ULONGLONG deadline = GetTickCount64() + 5000;
    do {
        queried = QueryInformationJobObject(job, JobObjectBasicAccountingInformation, &accounting, sizeof(accounting), nullptr) != FALSE;
        if (!queried || accounting.ActiveProcesses == 0) break;
        Sleep(20);
    } while (GetTickCount64() < deadline);
    std::fprintf(stderr, "CONTROL_JOB parent=%lu wallMs=%lu cpuLimit100ns=1200000000 cpuUsed100ns=%lld timedOut=%d exit=%lu activeAfterTermination=%lu\n",
        child.dwProcessId, wall, accounting.TotalUserTime.QuadPart, timedOut, code, accounting.ActiveProcesses);
    CloseHandle(child.hProcess); CloseHandle(job);
    return queried && accounting.ActiveProcesses == 0 ? static_cast<int>(code) : 125;
}

int wmain(int argc, wchar_t** argv) {
    wchar_t hosted[32]{}, actions[16]{};
    GetEnvironmentVariableW(L"RUNNER_ENVIRONMENT", hosted, 32);
    GetEnvironmentVariableW(L"GITHUB_ACTIONS", actions, 16);
    if (std::wcscmp(hosted, L"github-hosted") || std::wcscmp(actions, L"true")) return 2;
    if (argc >= 4 && std::wcscmp(argv[1], L"bounded") == 0) return bounded(argc, argv);
    if (argc == 6 && std::wcscmp(argv[1], L"placeholder") == 0) {
        CF_PLACEHOLDER_CREATE_INFO entry{};
        entry.RelativeFileName = argv[3];
        entry.FsMetadata.BasicInfo.FileAttributes = FILE_ATTRIBUTE_NORMAL;
        entry.FsMetadata.FileSize.QuadPart = std::stoll(argv[4]);
        entry.FileIdentity = argv[5];
        entry.FileIdentityLength = static_cast<DWORD>(std::wcslen(argv[5]) * sizeof(wchar_t));
        DWORD processed = 0;
        HRESULT hr = CfCreatePlaceholders(argv[2], &entry, 1, CF_CREATE_FLAG_NONE, &processed);
        std::printf("{\"hr\":\"%08lx\",\"entryHr\":\"%08lx\",\"processed\":%lu,\"size\":%lld}\n",
            static_cast<unsigned long>(hr), static_cast<unsigned long>(entry.Result), processed, entry.FsMetadata.FileSize.QuadPart);
        return FAILED(hr) || FAILED(entry.Result) ? 1 : 0;
    }
    if (argc == 4 && std::wcscmp(argv[1], L"hydrate") == 0) {
        HANDLE file = CreateFileW(argv[2], GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            nullptr, OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
        if (file == INVALID_HANDLE_VALUE) { std::printf("{\"openError\":%lu}\n", GetLastError()); return 1; }
        LARGE_INTEGER offset{}, length{}; length.QuadPart = std::stoll(argv[3]);
        HRESULT hr = CfHydratePlaceholder(file, offset, length, CF_HYDRATE_FLAG_NONE, nullptr);
        CloseHandle(file);
        std::printf("{\"hr\":\"%08lx\",\"requested\":%lld}\n", static_cast<unsigned long>(hr), length.QuadPart);
        return FAILED(hr) ? 1 : 0;
    }
    if (argc == 3 && std::wcscmp(argv[1], L"read") == 0) {
        HANDLE file = CreateFileW(argv[2], GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (file == INVALID_HANDLE_VALUE) { std::printf("{\"openError\":%lu}\n", GetLastError()); return 1; }
        LARGE_INTEGER size{}; BOOL sized = GetFileSizeEx(file, &size);
        char buffer[1024]; DWORD got = 0, error = 0; size_t total = 0;
        while (total <= 256 * 1024) {
            if (!ReadFile(file, buffer, sizeof(buffer), &got, nullptr)) { error = GetLastError(); break; }
            total += got; if (!got) break;
        }
        CloseHandle(file);
        std::printf("{\"sizeKnown\":%s,\"logicalBytes\":%lld,\"readBytes\":%zu,\"readError\":%lu}\n",
            sized ? "true" : "false", size.QuadPart, total, error);
        return error || !sized || total != static_cast<size_t>(size.QuadPart) ? 1 : 0;
    }
    if (argc == 3 && std::wcscmp(argv[1], L"stream") == 0) {
        std::ifstream input(std::filesystem::path(argv[2]), std::ios::binary);
        bool opened = input.is_open();
        std::vector<char> data(std::istreambuf_iterator<char>(input), {});
        std::printf("{\"opened\":%s,\"bytes\":%zu,\"bad\":%s,\"fail\":%s,\"eof\":%s}\n",
            opened ? "true" : "false", data.size(), input.bad() ? "true" : "false",
            input.fail() ? "true" : "false", input.eof() ? "true" : "false");
        return opened ? 0 : 1;
    }
    if (argc == 3 && std::wcscmp(argv[1], L"unlink-directory") == 0) {
        BOOL ok = RemoveDirectoryW(argv[2]);
        std::printf("{\"removed\":%s,\"error\":%lu}\n", ok ? "true" : "false", ok ? 0 : GetLastError());
        return ok ? 0 : 1;
    }
    return 2;
}
