/* Windows fixture: build with x86_64-w64-mingw32-gcc -DUNICODE -municode -mwindows
 * process-appid.c -o process-appid.exe -lshell32 -lole32 -lpropsys -luuid.
 * Run without arguments to open the test window. Pass a .lnk path and a
 * binary output path to generate a Taskband Favorites entry for the test pin.
 */
#define COBJMACROS
#include <windows.h>
#include <shellapi.h>
#include <shlobj.h>
#include <propsys.h>
#include <stdio.h>

static const PROPERTYKEY app_id = {
    {0x9f4c2855, 0x9f79, 0x4b39, {0xa8, 0xd0, 0xe1, 0xd4, 0x2d, 0xe1, 0xd5, 0xf3}}, 5
};

static HRESULT create_link(const wchar_t *path)
{
    wchar_t launcher[MAX_PATH], exe[MAX_PATH], args[MAX_PATH + 20];
    GetSystemDirectoryW(launcher, MAX_PATH);
    wcscat(launcher, L"\\cmd.exe");
    GetModuleFileNameW(NULL, exe, MAX_PATH);
    swprintf(args, MAX_PATH + 20, L"/c start \"\" \"%ls\"", exe);

    IShellLinkW *link = NULL;
    IPropertyStore *store = NULL;
    IPersistFile *file = NULL;
    HRESULT hr = CoCreateInstance(&CLSID_ShellLink, NULL, CLSCTX_INPROC_SERVER,
                                  &IID_IShellLinkW, (void **)&link);
    if (SUCCEEDED(hr))
        hr = IShellLinkW_SetPath(link, launcher);
    if (SUCCEEDED(hr))
        hr = IShellLinkW_SetArguments(link, args);
    if (SUCCEEDED(hr))
        hr = IShellLinkW_QueryInterface(link, &IID_IPropertyStore, (void **)&store);
    if (SUCCEEDED(hr)) {
        PROPVARIANT value = {0};
        value.vt = VT_LPWSTR;
        value.pwszVal = L"Explorust.Test.Group";
        hr = IPropertyStore_SetValue(store, &app_id, &value);
    }
    if (SUCCEEDED(hr))
        hr = IPropertyStore_Commit(store);
    if (SUCCEEDED(hr))
        hr = IShellLinkW_QueryInterface(link, &IID_IPersistFile, (void **)&file);
    if (SUCCEEDED(hr))
        hr = IPersistFile_Save(file, path, TRUE);

    if (file) IPersistFile_Release(file);
    if (store) IPropertyStore_Release(store);
    if (link) IShellLinkW_Release(link);
    return hr;
}

static int write_pin(const wchar_t *path, const wchar_t *output)
{
    if (FAILED(CoInitializeEx(NULL, COINIT_APARTMENTTHREADED)))
        return 1;
    PIDLIST_ABSOLUTE pidl = NULL;
    HRESULT hr = create_link(path);
    if (SUCCEEDED(hr))
        hr = SHParseDisplayName(path, NULL, &pidl, 0, NULL);
    int status = 1;
    if (SUCCEEDED(hr)) {
        FILE *file = _wfopen(output, L"wb");
        if (file) {
            UINT size = ILGetSize(pidl);
            int written = fputc(0, file) != EOF &&
                          fwrite(&size, sizeof size, 1, file) == 1 &&
                          fwrite(pidl, size, 1, file) == 1;
            status = fclose(file) != 0 || !written;
        }
    }
    CoTaskMemFree(pidl);
    CoUninitialize();
    return status;
}

static LRESULT CALLBACK window_proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY) {
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcW(hwnd, msg, wp, lp);
}

int WINAPI wWinMain(HINSTANCE instance, HINSTANCE previous, PWSTR command, int show)
{
    (void)previous;
    (void)command;
    int argc;
    wchar_t **argv = CommandLineToArgvW(GetCommandLineW(), &argc);
    if (!argv)
        return 1;
    if (argc > 1) {
        int result = argc == 3 ? write_pin(argv[1], argv[2]) : 1;
        LocalFree(argv);
        return result;
    }
    LocalFree(argv);

    if (FAILED(SetCurrentProcessExplicitAppUserModelID(L"explorust.test.group")))
        return 1;
    WNDCLASSW wc = {0};
    wc.lpfnWndProc = window_proc;
    wc.hInstance = instance;
    wc.lpszClassName = L"ExplorustProcessAppIdFixture";
    wc.hIcon = LoadIconW(NULL, IDI_INFORMATION);
    wc.hCursor = LoadCursorW(NULL, IDC_ARROW);
    wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    if (!RegisterClassW(&wc))
        return 1;
    HWND hwnd = CreateWindowExW(0, wc.lpszClassName, L"Process AUMID fixture",
                                WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT,
                                420, 280, NULL, NULL, instance, NULL);
    if (!hwnd)
        return 1;
    ShowWindow(hwnd, show);
    MSG msg;
    while (GetMessageW(&msg, NULL, 0, 0) > 0) {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    return 0;
}
