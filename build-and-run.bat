@echo off
rem 注意：本文件为 GBK(936) 编码，请勿转成 UTF-8 —— cmd 解析多字节会错位
setlocal EnableDelayedExpansion
rem ---------------------------------------------------------------------------
rem DataMind —— 一键「拉最新代码 → 打包 → 运行」
rem 用法：双击本文件即可；也支持命令行参数
rem   build-and-run.bat              拉取最新代码 + 打包 + 启动应用
rem   build-and-run.bat --skip-pull  跳过 git pull，直接用本地代码打包
rem   build-and-run.bat --installer 打包后运行 NSIS 安装程序（而非直接启动 exe）
rem ---------------------------------------------------------------------------

cd /d "%~dp0"

set "DO_PULL=1"
set "RUN_INSTALLER=0"

:parse_args
if "%~1"=="" goto args_done
if /i "%~1"=="--skip-pull" set "DO_PULL=0"
if /i "%~1"=="--installer" set "RUN_INSTALLER=1"
shift
goto parse_args
:args_done

echo ============================================================
echo  DataMind  一键打包并运行
echo ============================================================
echo  项目目录：%CD%
echo.

rem ---------- 0. 工具链检查 ----------
where git  >nul 2>&1 || (echo [FAIL] 未找到 git，请先安装 Git & goto failed)
where npm  >nul 2>&1 || (echo [FAIL] 未找到 npm，请先安装 Node.js & goto failed)
where cargo >nul 2>&1 || (echo [FAIL] 未找到 cargo，请先安装 Rust 工具链 & goto failed)

rem ---------- 1. 拉取最新代码 ----------
echo [1/4] 拉取最新代码 ...
if "%DO_PULL%"=="0" (
    echo       --skip-pull：已跳过，使用本地代码
) else (
    git rev-parse --is-inside-work-tree >nul 2>&1
    if errorlevel 1 (
        echo       当前目录不是 git 仓库，跳过拉取
    ) else (
        git pull --ff-only
        if errorlevel 1 (
            echo.
            echo [WARN] git pull 未成功：可能是离线、远程无权限、本地有未推送提交需要与远程合并。
            echo        将继续使用本地代码打包（如需严格以远程为准，请先手动处理后再运行本脚本）。
            choice /c YN /t 15 /d Y /m "        是否继续用本地代码打包"
            if errorlevel 2 goto aborted
        ) else (
            echo       已是最新代码
        )
    )
)
for /f "delims=" %%i in ('git rev-parse --short HEAD 2^>nul') do set "GIT_HEAD=%%i"
if defined GIT_HEAD echo       当前提交：%GIT_HEAD%
echo.

rem ---------- 2. 依赖检查 ----------
echo [2/4] 检查前端依赖 ...
if exist "node_modules" (
    echo       node_modules 已存在，跳过 npm install
) else (
    echo       首次运行，正在安装依赖（约 1-3 分钟）...
    call npm install
    if errorlevel 1 (echo [FAIL] npm install 失败 & goto failed)
)
echo.

rem ---------- 3. 打包 ----------
echo [3/4] 正在打包（Rust release 编译较慢，首次约 5-15 分钟，请耐心等待）...
call npm run tauri:build
if errorlevel 1 (echo [FAIL] 打包失败，请查看上方错误信息 & goto failed)
echo.

rem ---------- 4. 运行 ----------
echo [4/4] 启动 DataMind ...
set "APP_EXE=src-tauri\target\release\datamind.exe"
if "%RUN_INSTALLER%"=="1" (
    set "SETUP_EXE="
    for /f "delims=" %%f in ('dir /b /o-d "src-tauri\target\release\bundle\nsis\*_x64-setup.exe" 2^>nul') do (
        if not defined SETUP_EXE set "SETUP_EXE=src-tauri\target\release\bundle\nsis\%%f"
    )
    if not defined SETUP_EXE (echo [FAIL] 未找到安装包 & goto failed)
    echo       运行安装程序：%SETUP_EXE%
    start "" "%SETUP_EXE%"
) else (
    if not exist "%APP_EXE%" (echo [FAIL] 未找到 %APP_EXE% & goto failed)
    start "" "%APP_EXE%"
)

echo.
echo ============================================================
echo  完成：已打包并启动。
echo  · 应用本体：%CD%\%APP_EXE%
echo  · 安装包目录：%CD%\src-tauri\target\release\bundle\nsis\
echo ============================================================
echo.
pause
exit /b 0

:aborted
echo.
echo 已取消。
pause
exit /b 1

:failed
echo.
echo 打包未成功完成，请修复上方错误后重新运行本脚本。
pause
exit /b 1
