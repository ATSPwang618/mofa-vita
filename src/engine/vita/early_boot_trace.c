#include <psp2/io/dirent.h>
#include <psp2/io/fcntl.h>
#include <psp2/io/stat.h>

#include <stddef.h>

static const char k_boot_trace_path[] = "ux0:data/mofa-vita/boot-status.txt";
static const char k_boot_trace_fallback_path[] = "ux0:data/mofa-vita-boot-status.txt";
static const char k_error_path[] = "ux0:data/mofa-vita/error.txt";
static const char k_error_fallback_path[] = "ux0:data/mofa-vita-error.txt";

/*
 * 每个阶段标记后面补一行中文释义，文件仍然保留原始的英文标记：
 * cmake/VerifyHardwareBootTrace.cmake 用整行精确匹配这些标记，
 * 所以释义必须另起一行并以 '#' 开头，测试脚本看到的就是同一份追踪。
 */
typedef struct { const char *stage; const char *zh; } mofa_boot_glossary_t;

static const mofa_boot_glossary_t k_boot_glossary[] = {
	{"preinit-entered", "进入静态初始化之前的第一个写入点（本次启动开始）"},
	{"main-entered", "进入应用主函数"},
	{"vita-newlib-heap-128m", "已把 newlib 堆扩展到 128MiB"},
	{"vita-main-thread-stack-2m", "已把主线程栈设为 2MiB"},
	{"vita-main-thread-policy-ready", "主线程调度策略设置完成"},
	{"vita-threading-self-test-entered", "线程自检开始"},
	{"vita-threading-self-test-passed", "线程自检通过"},
	{"fatal-error-written", "已写入致命错误（详见 error.txt）"},
	{"retail-launch-resolved", "已确定要启动的游戏工程"},
	{"retail-phase1-heuristic-entered", "开始第一阶段过滤器推断"},
	{"retail-phase1-heuristic-identity", "过滤器推断结果：无需解密"},
	{"retail-phase1-heuristic-selected", "过滤器推断已选定规则"},
	{"retail-phase1-heuristic-verified", "过滤器推断已自校验通过"},
	{"retail-xp3filter-selected", "已选定游戏自带的 xp3filter.tjs"},
	{"retail-xp3filter-embedded-selected", "已选用本体内置的兼容 xp3filter.tjs"},
	{"retail-startup-patch-selected", "已选定启动补丁"},
	{"retail-startup-patch-embedded-selected", "已选用本体内置的启动补丁"},
	{"retail-patch-game-directory-fallback", "补丁回退到游戏目录"},
	{"retail-patch-resolution-failed", "补丁解析失败"},
	{"retail-project-root-selected", "已选定工程根目录"},
	{"retail-project-data-xp3-selected", "已选定 data.xp3 作为主档案"},
	{"retail-project-content-data-selected", "已选定内容数据目录"},
	{"retail-project-data-directory-selected", "已选定数据目录"},
	{"retail-project-data-exe-selected", "已选定可执行文件作为工程线索"},
	{"retail-project-executable-identified", "已识别游戏可执行文件"},
	{"retail-project-executable-not-identified", "未能识别游戏可执行文件（改用目录启发式）"},
	{"retail-windows-plugin-skipped", "跳过了一个 Windows 专用插件（Vita 端不加载）"},
	{"retail-layereximage-ready", "插件 layerExImage.dll 已注册（图像滤镜/缩放）"},
	{"retail-layerexareaaverage-ready", "插件 layerExAreaAverage.dll 已注册（区域平均缩小）"},
	{"retail-layerexfilter-load-only-ready", "插件 layerExFilter.dll 已登记为“仅可链接”（滤镜像素未实现）"},
	{"retail-layerexparticle-load-only-ready", "插件 layerExParticle.dll 已登记为“仅可链接”（粒子未实现）"},
	{"retail-layerexyadraw-load-only-ready", "插件 layerExYaDraw.dll 已登记为“仅可链接”（集中线未实现）"},
	{"retail-filter-load-only-ready", "插件 filter.dll 已登记为“仅可链接”（滤镜像素未实现）"},
	{"retail-clipboardex-load-only-ready", "插件 clipboardEx.dll 已登记为“仅可链接”（无系统剪贴板）"},
	{"retail-windowex-load-only-ready", "插件 windowEx.dll 已登记为“仅可链接”（无 Win32 窗口）"},
	{"retail-stringutil-load-only-ready", "插件 stringUtil.dll 已登记为“仅可链接”（isNumber/initSpline 未实现）"},
	{"retail-equations-load-only-ready", "插件 Equations.dll 已登记为“仅可链接”（补丁用 TJS 版 Equations 类替代）"},
	{"retail-json-load-only-ready", "插件 json.dll 已登记为“仅可链接”（引擎自带 Scripts.evalJSON 可用）"},
	{"retail-snow3d-load-only-ready", "插件 snow3d.dll 已登记为“仅可链接”（降雪效果未实现）"},
	{"retail-messenger-load-only-ready", "插件 messenger.dll 已登记为“仅可链接”（调试用消息接收）"},
	{"retail-systemextouchimage-load-only-ready", "插件 SystemExTouchImage.dll 已登记为“仅可链接”（图像缓存）"},
	{"retail-base64-load-only-ready", "插件 base64.dll 已登记为“仅可链接”"},
	{"retail-qrcode-load-only-ready", "插件 qrcode.dll 已登记为“仅可链接”"},
	{"retail-registory-load-only-ready", "插件 registory.dll 已登记为“仅可链接”（注册表访问）"},
	{"retail-win32ole-load-only-ready", "插件 win32ole.dll 已登记为“仅可链接”（OLE 自动化）"},
	{"retail-layerexbtoa-surface-ready", "插件 layerExBTOA.dll 已注册（透明通道/选区）"},
	{"retail-layerexdraw-surface-ready", "插件 layerExDraw.dll 已注册（绘制接口，像素为空实现）"},
	{"retail-layerexsave-ready", "插件 layerExSave.dll 已注册（截图/存档图）"},
	{"retail-scriptsex-surface-ready", "插件 scriptsEx.dll 已注册"},
	{"retail-motionplayer-surface-ready", "插件 motionplayer.dll 已注册（PSB 解析，渲染为空实现）"},
	{"retail-gfxeffect-fallback-ready", "插件 gfxEffect.dll 已注册（状态保留，火焰为空实现）"},
	{"retail-extnagano-crossfade-fallback-ready", "插件 extNagano.dll 已用交叉淡入替代"},
	{"retail-extrans-ready", "插件 extrans.dll 已注册（转场）"},
	{"retail-shrinkcopy-ready", "插件 shrinkCopy.dll 已注册"},
	{"retail-fstat-ready", "插件 fstat.dll 已注册（文件/目录扩展 API）"},
	{"retail-sqlite3-ready", "插件 sqlite3.dll 已注册"},
	{"retail-psbfile-ready", "插件 psbfile.dll 已注册"},
	{"retail-sigcheck-rsa-pss-ready", "签名校验已注册"},
	{"retail-krmovie-core-alias-ready", "krmovie.dll 由引擎自带视频后端接管"},
	{"retail-vita-afterstartup-executed", "启动脚本已执行到 AfterStartup"},
	{"vitagl-init-entered", "开始初始化 vitaGL（图形后端）"},
	{"vitagl-shader-compiler-found", "找到可用的着色器编译器"},
	{"vitagl-garbage-collector-configured", "vitaGL 显存回收已配置"},
	{"vitagl-cached-ram-pool-enabled", "vitaGL 缓存显存池已启用"},
	{"vitagl-init-returned", "vitaGL 初始化返回"},
	{"vitagl-initialized", "vitaGL 初始化完成"},
	{"vitagl-swap-decoupled-from-engine-tick", "画面交换与引擎计时解耦"},
	{"vitagl-bootstrap-frame-presented", "已呈现第一帧（自检画面）"},
	{"vitagl-presentation-texture-ready", "呈现用纹理已建立"},
	{"vitagl-first-game-frame-entered", "开始提交第一张游戏画面"},
	{"vitagl-first-game-frame-presented", "第一张游戏画面已呈现"},
	{"yuri-platform-ready", "平台层就绪"},
	{"yuri-start-application-entered", "开始执行引擎的应用程序初始化"},
	{"yuri-start-application-returned", "引擎应用程序初始化返回"},
	{"yuri-script-engine-initialized", "TJS 脚本引擎已初始化"},
	{"yuri-project-normalized", "工程路径已规范化"},
	{"yuri-fonts-initialized", "字体系统已初始化"},
	{"yuri-msgothic-collection-parsed", "已解析 msgothic 字体集合"},
	{"yuri-msgothic-primary-selected", "已选定主字体"},
	{"yuri-eager-layer-cache-release-enabled", "已启用图层缓存及时释放"},
	{"yuri-base-systems-initialized", "引擎基础子系统已初始化"},
	{"yuri-system-app-id-compat-ready", "System 应用标识兼容层就绪"},
	{"yuri-application-initialized", "引擎 Application 已初始化"},
	{"yuri-storage-preflight-entered", "进入存储预检（检查 XP3 与补丁）"},
	{"yuri-storage-preflight-complete", "存储预检完成"},
	{"yuri-project-directory-enumerated", "已枚举工程目录"},
	{"yuri-project-xp3-opened", "已打开工程 XP3 档案"},
	{"yuri-xp3filter-opened", "已加载 xp3filter.tjs"},
	{"yuri-patch-opened", "已加载游戏补丁（patch.tjs）"},
	{"yuri-internal-plugins-ready", "引擎内置插件注册完成"},
	{"yuri-startup-storage-preflight-entered", "进入启动脚本存储预检"},
	{"yuri-startup-storage-preflight-complete", "启动脚本存储预检完成"},
	{"yuri-startup-storage-opened", "已打开启动脚本存储"},
	{"yuri-system-initialized", "System 初始化完成"},
	{"yuri-system-control-created", "System 控制对象已创建"},
	{"yuri-startup-script-entered", "开始执行游戏启动脚本（进入 KAG）"},
	{"yuri-additive-alpha-scalar-exact-ready", "加法混合的 alpha 定标已精确实现"},
	{"yuri-tvpgl-neon-kernels-installed", "已安装 NEON 加速的图像内核（真机默认；Vita3K 可能无法执行）"},
	{"yuri-tvpgl-scalar-kernels", "已改用通用 C++ 图像内核（存在 ux0:data/mofa-vita/tvpgl-scalar 标记）"},
	{"yuri-tvpgl-kernels-measured-on-device", "已在运行时实测 NEON 与通用内核的快慢，只保留更快的那一版"},
	{"yuri-render-tasks-hybrid-large", "渲染任务采用大图混合策略"},
	{"yuri-render-task-pool-ready", "渲染任务线程池就绪"},
	{"yuri-software-framebuffer-ready", "软件帧缓冲就绪"},
	{"yuri-event-loop-entered", "进入事件循环"},
	{"yuri-event-loop-exited", "退出事件循环"},
	{"yuri-first-window-created", "第一个窗口已创建"},
	{"yuri-text-write-buffered-ready", "文本写入缓冲就绪"},
	{"yuri-msgothic-freetype-rasterizer-selected", "已选用 FreeType 渲染 msgothic 字体"},
	{"yuri-software-static-texture-direct-ready", "静态纹理可直接上传"},
	{"yuri-opencv-software-fastpaths-ready", "OpenCV 兼容层的软件快速路径就绪"},
	{"yuri-bitmap-tiered-allocator-ready", "位图分级分配器就绪"},
	{"yuri-bitmap-user-rw-memblocks-ready", "位图读写内存块就绪"},
	{"yuri-direct-texture-fallback", "纹理采用直接回退路径"},
	{"yuri-kag-debug-log-disabled", "已关闭 KAG 调试日志"},
	{"yuri-timer-thread-entered", "计时线线程已启动"},
	{"yuri-timer-thread-policy-ready", "计时线线程策略设置完成"},
	{"yuri-msgothic-pread-stream-ready", "msgothic 预读流就绪"},
	{"yuri-msgothic-pread-cache-ready", "msgothic 预读缓存就绪"},
	{"yuri-msgothic-freetype-face-applied", "已对 FreeType 应用 msgothic 字面（face）"},
	{"yuri-startup-script-complete", "游戏启动脚本执行完毕"},
	{"yuri-presentation-reference-ready", "呈现参考（尺寸/格式）就绪"},
	{"vita-touch-history-reader-started", "触摸历史读取线程已启动"},
	{"vitagl-first-game-frame-source-contentful", "第一张画面已有实际内容"},
	{"vitagl-first-game-frame-uploaded", "第一张画面已上传到显存"},
	{"vitagl-first-game-frame-arrays-ready", "第一张画面的顶点/索引数组已就绪"},
	{"yuri-openal-initialized", "OpenAL 音频已初始化"},
	{"yuri-audio-first-buffer-queued", "首个音频缓冲已入队"},
	{"yuri-audio-first-play-started", "音频开始播放"},
	{"yuri-ffmpeg-movie-backend-ready", "FFmpeg 视频后端就绪"},
	{"yuri-first-movie-frame-decoded", "首个视频帧已解码"},
	{"yuri-first-movie-audio-decoded", "首个视频音频已解码"},
	{"retail-runtime-5s-stable-with-video", "稳定运行 5 秒（含视频）"},
	{"retail-runtime-30s-stable-with-video", "稳定运行 30 秒（含视频）"},
};

static int text_equal(const char *left, const char *right)
{
	if(!left || !right) return 0;
	while(*left && *right)
	{
		if(*left != *right) return 0;
		++left;
		++right;
	}
	return *left == *right;
}

static const char *boot_glossary_lookup(const char *stage)
{
	size_t index;
	if(!stage) return 0;
	for(index = 0; index < sizeof(k_boot_glossary) / sizeof(k_boot_glossary[0]); ++index)
	{
		if(text_equal(k_boot_glossary[index].stage, stage))
			return k_boot_glossary[index].zh;
	}
	return 0;
}

static size_t text_length(const char *text)
{
	size_t length = 0;
	if(!text) return 0;
	while(text[length] != '\0') ++length;
	return length;
}

static size_t append_text(char *buffer, size_t capacity, size_t used,
	const char *text)
{
	if(!text) return used;
	while(*text != '\0' && used + 1 < capacity)
		buffer[used++] = *text++;
	buffer[used] = '\0';
	return used;
}

static int text_contains(const char *haystack, const char *needle)
{
	size_t offset = 0;
	if(!haystack || !needle) return 0;
	while(haystack[offset] != '\0')
	{
		size_t index = 0;
		while(needle[index] != '\0' && haystack[offset + index] == needle[index])
			++index;
		if(needle[index] == '\0') return 1;
		++offset;
	}
	return 0;
}

static void write_text(const char *path, const char *fallback_path,
	const char *text, int flags)
{
	SceUID file;
	const size_t length = text_length(text);

	sceIoMkdir("ux0:data/mofa-vita", 0777);
	if(flags & SCE_O_TRUNC)
	{
		/*
		 * Vita3K has been observed to keep the tail of a longer previous
		 * message when a shorter one is written with SCE_O_TRUNC, which mixes
		 * two runs into one diagnostic file.  Remove the file first so
		 * error.txt and boot-status.txt always describe the current run only.
		 */
		sceIoRemove(path);
		sceIoRemove(fallback_path);
	}
	file = sceIoOpen(path, SCE_O_WRONLY | SCE_O_CREAT | flags, 0666);
	if(file < 0)
		file = sceIoOpen(fallback_path, SCE_O_WRONLY | SCE_O_CREAT | flags, 0666);
	if(file < 0) return;
	if(length > 0) sceIoWrite(file, text, length);
	sceIoWrite(file, "\n", 1);
	sceIoSyncByFd(file, 0);
	sceIoClose(file);
}

void mofa_boot_trace(const char *stage)
{
	const char *gloss;

	write_text(k_boot_trace_path, k_boot_trace_fallback_path, stage,
		SCE_O_APPEND);

	gloss = boot_glossary_lookup(stage);
	if(gloss)
	{
		char line[360];
		size_t used = 0;
		used = append_text(line, sizeof(line), used, "# 说明：");
		used = append_text(line, sizeof(line), used, gloss);
		write_text(k_boot_trace_path, k_boot_trace_fallback_path, line,
			SCE_O_APPEND);
	}
}

void mofa_write_error(const char *message)
{
	static char buffer[4096];
	size_t used = 0;

	buffer[0] = '\0';
	used = append_text(buffer, sizeof(buffer), used,
		"【mofa-vita-krkr 启动错误】\n");
	used = append_text(buffer, sizeof(buffer), used,
		"以下是引擎/游戏脚本给出的原始信息（保留原文便于对照）：\n");
	used = append_text(buffer, sizeof(buffer), used,
		message ? message : "未知的启动错误");
	used = append_text(buffer, sizeof(buffer), used, "\n");
	if(text_contains(message, "Cannot load Plugin"))
	{
		used = append_text(buffer, sizeof(buffer), used,
			"【原因】脚本用 Plugins.link() 链接了一个 Windows 专用 DLL。"
			"Vita 端不加载 Windows DLL，也不会自动等价实现它。\n"
			"【处理】\n"
			"  1. 只是无条件链接时：把模块名登记进 "
			"include/mofa/yuri_plugin_capabilities.hpp 的 internal 列表，"
			"并在 src/engine/retail/ 下给出同名模块，由 ncbAutoRegister 静态注册；\n"
			"  2. 脚本随后要调用它的方法时：还需要在 Vita 侧写出对应的原生实现，"
			"否则只能算“可链接”，会把缺失效果如实报为兼容性缺口。\n");
	}

	write_text(k_error_path, k_error_fallback_path, buffer, SCE_O_TRUNC);
}

static void mofa_preinit_trace(void)
{
	write_text(k_boot_trace_path, k_boot_trace_fallback_path,
		"=== mofa-vita-krkr 启动追踪（本次运行） ===\n"
		"每行英文是程序内部的阶段标记；紧跟其后以 # 开头的一行是中文说明。\n"
		"preinit-entered",
		SCE_O_TRUNC);
}

/*
 * Vita newlib runs .preinit_array before every C++ global constructor.  This
 * gives us a durable boundary between a loader/import failure and a crash in
 * static initialization, without relying on SDL, libc stdio, or the engine.
 */
__attribute__((section(".preinit_array"), used))
static void (*const mofa_preinit_entry)(void) = mofa_preinit_trace;
