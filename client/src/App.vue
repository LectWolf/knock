<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

/* 桌面壳里跑的时候，状态和命令都来自 Rust；用浏览器打开时退回演示模式（URL 参数摆状态） */
const shell: any = (window as any).__TAURI__;
const invoke: ((cmd: string, args?: any) => Promise<any>) | undefined = shell?.core?.invoke;
const isShell = typeof invoke === "function";

/* 状态机是 Rust 那边的唯一真相，界面只渲染它。
   这一版还没接 Tauri 命令，先用 URL 参数把各状态摆出来给人看；
   接上以后 handle() 会被 transport 推来的 Update 替换掉。 */
type ConnState =
  | "idle"
  | "creating"
  | "waiting"
  | "punching"
  | "direct"
  | "relay"
  | "failed";

type FailReason =
  | "code_not_found"
  | "host_offline"
  | "blocked_by_firewall"
  | "symmetric_nat"
  | "relay_unavailable";

type Theme = "auto" | "light" | "dark";

const params = new URLSearchParams(location.search);
const pick = <T extends string>(key: string, allowed: readonly T[], fallback: T): T => {
  const v = params.get(key);
  return v && (allowed as readonly string[]).includes(v) ? (v as T) : fallback;
};

const winMin = () => void invoke?.("win_minimize");
/* 自己实现拖窗：记下起点的窗口位置和鼠标屏幕坐标，鼠标动多少窗口就动多少 */
let dragging: { x: number; y: number; wx: number; wy: number } | null = null;
const startDrag = async (e: MouseEvent) => {
  if (!isShell || e.button !== 0) return;
  const pos = await invoke?.("win_pos").catch(() => null);
  if (!pos) return;
  dragging = { x: e.screenX, y: e.screenY, wx: pos[0], wy: pos[1] };
  window.addEventListener("mousemove", onDrag);
  window.addEventListener("mouseup", endDrag, { once: true });
};
const onDrag = (e: MouseEvent) => {
  if (!dragging) return;
  const x = dragging.wx + (e.screenX - dragging.x);
  const y = dragging.wy + (e.screenY - dragging.y);
  void invoke?.("win_set_pos", { x, y });
};
const endDrag = () => {
  dragging = null;
  window.removeEventListener("mousemove", onDrag);
};
const mode = ref<"none" | "knock" | "open">(pick("mode", ["none", "knock", "open"], "none"));
const pick_ = (m: "knock" | "open") => {
  mode.value = mode.value === m ? "none" : m;
};
const winHide = () => void invoke?.("win_hide");

const state = ref<ConnState>(
  pick("state", ["idle", "creating", "waiting", "punching", "direct", "relay", "failed"], "idle"),
);
const fail = ref<FailReason>(
  pick(
    "fail",
    ["code_not_found", "host_offline", "blocked_by_firewall", "symmetric_nat", "relay_unavailable"],
    "host_offline",
  ),
);
const theme = ref<Theme>(pick("theme", ["auto", "light", "dark"], "auto"));

const code = ref(params.get("code") ?? "······");

/* 昵称：首次启动必须设，之后在设置里改 */
const nickname = ref(params.get("nickname") ?? "");
const firstRun = ref(params.get("first") === "1" || (params.get("state") ?? "idle") === "idle" ? !nickname.value : false);

/* 门里的人。接上 Rust 的会话列表后，这里换成订阅推送 */
type Guest = { id: string; nick: string; mode: "direct" | "relay"; since: string };
const parseGuests = (raw: string | null): Guest[] =>
  (raw ?? "")
    .split(",")
    .filter(Boolean)
    .map((one) => {
      const [nick, mode, since] = one.split(":");
      return {
        id: nick,
        nick,
        mode: mode === "relay" ? "relay" : "direct",
        since: since ?? "刚刚",
      };
    });
const guests = ref<Guest[]>(parseGuests(params.get("guests")));
const kick = (id: string) => {
  guests.value = guests.value.filter((g) => g.id !== id);
};

/* 和 Rust 对齐：状态由 transport 推来，名册每秒拉一次 */
const loadSettings = async () => {
  if (!invoke) return;
  const s = await invoke("load_settings").catch(() => null);
  if (!s) return;
  nickname.value = s.nickname ?? "";
  server.value = String(s.port ?? 25565);
  theme.value = (s.theme as Theme) ?? "auto";
  relayPolicy.value = s.relay ?? "auto";
  nodeAddr.value = s.node ?? "";
  if (s.nickname) firstRun.value = false;
};

const persist = async () => {
  if (!invoke) return;
  await invoke("save_settings", {
    settings: {
      nickname: nickname.value,
      node: nodeAddr.value,
      port: Number(server.value) || 25565,
      relay: relayPolicy.value,
      theme: theme.value,
    },
  }).catch(() => {});
};

const trackOpenTimer = () => {
  if (state.value === "waiting" && openedAt.value === null) openedAt.value = Date.now();
  if (state.value !== "waiting" && openedAt.value !== null) openedAt.value = null;
};

const pull = async () => {
  if (!invoke) return;
  const s = await invoke("door_state").catch(() => null);
  if (s) state.value = s as ConnState;
  const c = await invoke("door_code").catch(() => null);
  if (typeof c === "string" && c) code.value = c.toUpperCase();
  trackOpenTimer();
  const gs = await invoke("guests").catch(() => null);
  if (Array.isArray(gs)) {
    guests.value = gs.map((g: any) => ({
      id: g.id,
      nick: g.nickname,
      mode: g.via_relay ? "relay" : "direct",
      since: g.secs < 60 ? `${g.secs} 秒` : `${Math.floor(g.secs / 60)} 分钟`,
    }));
  }
};

onMounted(async () => {
  if (!isShell) return;
  await loadSettings();
  await pull();
  shell?.event?.listen?.("knock://state", (e: any) => {
    state.value = e.payload.state as ConnState;
    if (e.payload.fail) fail.value = e.payload.fail;
    if (e.payload.state === "waiting" && openedAt.value === null) openedAt.value = Date.now();
    if (e.payload.state !== "waiting") openedAt.value = null;
  });
  shell?.event?.listen?.("knock://tray", (msg: string) => {
    if (msg === "close_door") void closeDoor();
  });
  window.setInterval(pull, 1000);
});

const showSettings = ref(params.get("panel") === "settings");
const nodeAddr = ref(params.get("node") ?? "");
const viewKey = computed(() =>
  firstRun.value ? "first" : showSettings.value ? "settings" : state.value + (state.value === "failed" ? ":" + fail.value : ""),
);
const relayPolicy = ref(params.get("relay") ?? "auto");
const input = ref(params.get("input") ?? "");
const server = ref("25565");

/* —— 主题：默认跟随系统，可手动覆盖 —— */
watch(
  theme,
  (t) => {
    const root = document.documentElement;
    if (t === "auto") delete root.dataset.theme;
    else root.dataset.theme = t;
  },
  { immediate: true },
);
const themeLabel = computed(() => (theme.value === "auto" ? "跟随系统" : theme.value === "light" ? "亮色" : "暗色"));
const nextTheme = () => (theme.value = theme.value === "auto" ? "light" : theme.value === "light" ? "dark" : "auto");

/* 门牌号只保留 Crockford 字母表里的字符，转大写 */
const ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const cleanCode = (raw: string) =>
  raw
    .toUpperCase()
    .replace(/[O]/g, "0")
    .replace(/[IL]/g, "1")
    .split("")
    .filter((c) => ALPHABET.includes(c))
    .slice(0, 6)
    .join("");
watch(input, (v) => {
  const c = cleanCode(v);
  if (c !== v) input.value = c;
});
const canKnock = computed(() => input.value.length === 6);

const cleanNick = computed(() => nickname.value.trim().slice(0, 24));
const canSaveNick = computed(() => cleanNick.value.length > 0);
const saveNick = async () => {
  nickname.value = cleanNick.value;
  if (isShell) await persist();
  firstRun.value = false;
};

/* —— 文案：全部走「门」这一套，不许出现网络术语 —— */
const failCopy: Record<FailReason, { title: string; hint: string }> = {
  code_not_found: { title: "门牌号不对", hint: "可能敲错了一位，也可能门已经关了。" },
  host_offline: { title: "门开着，但屋里没人", hint: "他还没把服务开起来，让他先开服再敲一次。" },
  blocked_by_firewall: { title: "门被挡住了", hint: "本机的防护软件挡住了出门的路，放行一次就好。" },
  symmetric_nat: { title: "两边都出不去", hint: "这条路走不通，需要在设置里允许绕路。" },
  relay_unavailable: { title: "绕路也走不通", hint: "过一会儿再试，或者换一个门牌号。" },
};

const status = computed(() => {
  switch (state.value) {
    case "creating":
      return { tone: "busy", label: "正在开门…", hint: "马上就好" };
    case "punching":
      return { tone: "busy", label: "正在敲门…", hint: "在找一条能过去的路" };
    case "direct":
      return { tone: "ok", label: "门开了", hint: "直接连到他那边，这条路上没有别人" };
    case "relay":
      return { tone: "warn", label: "门开了", hint: "绕了点路，会慢一点" };
    case "failed":
      return { tone: "err", label: failCopy[fail.value].title, hint: failCopy[fail.value].hint };
    default:
      return { tone: "idle", label: "", hint: "" };
  }
});

/* 门开了多久 / 关门 */
/* 门开了多久：记录进入 waiting 的真实时刻，不认 URL 参数 */
const openedAt = ref<number | null>(null);
const now = ref(Date.now());
window.setInterval(() => (now.value = Date.now()), 1000);
const opened = computed(() =>
  openedAt.value === null ? 0 : Math.floor((now.value - openedAt.value) / 60000),
);
const confirmClose = ref(false);
const closeDoor = async () => {
  confirmClose.value = false;
  if (isShell) await invoke!("close_door").catch(() => {});
  state.value = "idle";
  openedAt.value = null;
};

const copied = ref(false);
let timer: number | undefined;
const copyCode = async () => {
  try {
    await navigator.clipboard.writeText(code.value);
  } catch {
    /* 剪贴板不可用时也别打断用户 */
  }
  copied.value = true;
  window.clearTimeout(timer);
  timer = window.setTimeout(() => (copied.value = false), 1600);
};
onBeforeUnmount(() => window.clearTimeout(timer));

const warn = ref("");

const knock = async () => {
  if (!canKnock.value) return;
  warn.value = "";
  if (isShell) {
    try {
      await invoke!("knock", { invite: input.value });
    } catch (e: any) {
      warn.value = explain(e);
    }
    return;
  }
  state.value = "punching";
};

/* 把 Rust 那边的中文错误原样露出来；技术错误归到一句人话 */
const explain = (e: any) => {
  const raw = typeof e === "string" ? e : (e?.message ?? "");
  if (raw.includes("先添加一个节点")) return "还没填节点，去设置里加一个";
  if (raw.includes("IllegalEvent")) return "现在这个状态没法做这件事";
  return raw ? "没做成，稍后再试" : "没做成，稍后再试";
};
const open = async () => {
  warn.value = "";
  if (isShell) {
    try {
      await invoke!("open_door", { port: Number(server.value) || 25565 });
    } catch (e: any) {
      warn.value = explain(e);
      state.value = "idle";
    }
    return;
  }
  state.value = "creating";
};
const retry = () => {
  state.value = "idle";
};
</script>

<template>
  <div class="shell">
    <header class="bar" @mousedown="startDrag">
      <div class="mark">
        <span class="dot" aria-hidden="true"></span>
        <span class="word">Knock</span>
      </div>
      <div class="win-btns">
        <button class="win-btn" type="button" aria-label="最小化" title="最小化" @click.stop="winMin">
          <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M3 6.5h6" /></svg>
        </button>
        <button class="win-btn close" type="button" aria-label="收进托盘" title="收进托盘" @click.stop="winHide">
          <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M3.5 3.5l5 5M8.5 3.5l-5 5" /></svg>
        </button>
      </div>
    </header>

    <main class="stage" :data-tone="status.tone">
      <Transition name="view" mode="out-in">
      <div class="view" :key="viewKey">
      <!-- 首次启动：先起个名字 -->
      <template v-if="firstRun">
        <h1 class="title">给自己起个名字</h1>
        <p class="lede">朋友敲门时，看到的就是这个名字。之后可以随时改。</p>
        <label class="field">
          <span class="field-label">昵称</span>
          <input
            v-model="nickname"
            class="text-input"
            maxlength="24"
            placeholder="比如：小张"
            autocomplete="off"
          />
        </label>
        <button class="primary" type="button" :disabled="!canSaveNick" @click="saveNick">
          就这样
        </button>
      </template>

      <!-- 设置 -->
      <template v-else-if="showSettings">
        <h1 class="title">设置</h1>
        <div class="settings">
          <label class="row-setting">
            <span class="setting-label">昵称</span>
            <input v-model="nickname" class="small-input" maxlength="24" />
          </label>
          <div class="row-setting">
            <span class="setting-label">打不通时</span>
            <div class="seg">
              <button
                v-for="opt in [
                  { id: 'auto', label: '自动绕路' },
                  { id: 'ask', label: '问我' },
                  { id: 'never', label: '不绕路' },
                ]"
                :key="opt.id"
                type="button"
                class="seg-btn"
                :class="{ on: relayPolicy === opt.id }"
                @click="relayPolicy = opt.id"
              >
                {{ opt.label }}
              </button>
            </div>
          </div>
          <label class="row-setting wide">
            <span class="setting-label">节点</span>
            <input
              v-model="nodeAddr"
              class="small-input grow mono"
              placeholder="http://节点地址:30990"
              spellcheck="false"
              @change="persist"
            />
          </label>
          <label class="row-setting">
            <span class="setting-label">外观</span>
            <button class="ghost tiny" type="button" @click="nextTheme">{{ themeLabel }}</button>
          </label>
        </div>
        <button class="ghost" type="button" @click="showSettings = false">返回</button>
      </template>

      <!-- 未开始：输入门牌号 -->
      <template v-else-if="state === 'idle'">
        <div class="choice" :class="{ picked: mode !== 'none' }">
          <h1 class="title">今天要哪一头</h1>
          <p class="lede">开门的人发号，敲门的人输号。</p>

          <div class="choice-row">
            <button
              class="choice-card"
              type="button"
              :class="{ on: mode === 'knock' }"
              @click="pick_('knock')"
            >
              <span class="choice-name">敲门</span>
              <span class="choice-sub">拿到号就能进</span>
            </button>
            <button
              class="choice-card"
              type="button"
              :class="{ on: mode === 'open' }"
              @click="pick_('open')"
            >
              <span class="choice-name">我要开门</span>
              <span class="choice-sub">我开了服，等朋友</span>
            </button>
          </div>
        </div>

        <div class="expander" :class="{ open: mode !== 'none' }">
          <div class="expander-inner">
            <div v-if="mode === 'knock'" class="panel-inline">
              <label class="field">
                <span class="field-label">门牌号</span>
                <input
                  v-model="input"
                  class="code-input mono"
                  inputmode="latin"
                  autocomplete="off"
                  spellcheck="false"
                  maxlength="6"
                  placeholder="A7K2M9"
                />
              </label>
              <p v-if="warn" class="warn">{{ warn }}</p>
              <button class="primary" type="button" :disabled="!canKnock" @click="knock">敲门</button>
            </div>

            <div v-else class="panel-inline">
              <label class="field">
                <span class="field-label">你的服务在</span>
                <input
                  v-model="server"
                  class="code-input port-input mono"
                  inputmode="numeric"
                  maxlength="5"
                  placeholder="25565"
                />
              </label>
              <p class="note center">没改过就是 25565。开好门把这串号发给他。</p>
              <p v-if="warn" class="warn">{{ warn }}</p>
              <button class="primary" type="button" @click="open">开门</button>
            </div>
          </div>
        </div>
      </template>

      <!-- 房主：正在开门 / 门开着 -->
      <template v-else-if="state === 'creating' || state === 'waiting'">
        <div class="pulse" v-if="state === 'creating'" aria-hidden="true"></div>

        <template v-if="state === 'waiting'">
          <div class="open-meta">
            <span class="live-dot" aria-hidden="true"></span>
            <span>门开着</span>
            <span class="dot-sep">·</span>
            <span class="mono">{{ opened }} 分钟</span>
          </div>
          <p class="eyebrow">门牌号</p>
          <div class="code-display mono">{{ code }}</div>
          <div class="row">
            <button class="ghost" type="button" @click="copyCode">
              {{ copied ? "已复制" : "复制门牌号" }}
            </button>
            <span class="note inline">你在，门就开着</span>
          </div>
          <div class="guests">
            <div class="guests-head">
              <span>门里的人</span>
              <span class="count mono">{{ guests.length }}</span>
            </div>
            <p v-if="guests.length === 0" class="note">还没人敲，门一直开着。</p>
            <TransitionGroup v-else tag="ul" name="guest" class="guest-list">
              <li v-for="g in guests" :key="g.id" class="guest">
                <span class="guest-name">{{ g.nick }}</span>
                <span class="badge tiny" :class="g.mode">
                  <span class="badge-dot"></span>{{ g.mode === "relay" ? "绕路" : "直连" }}
                </span>
                <span class="guest-since mono">{{ g.since }}</span>
                <button class="kick" type="button" @click="kick(g.id)">请出去</button>
              </li>
            </TransitionGroup>
            <p v-if="guests.length > 4" class="more-note">
              还有 {{ guests.length - 4 }} 人，往下滚
            </p>
          </div>
          <p class="hint">把门牌号发给他，他就能进来。你在门就开着。</p>

          <div v-if="!confirmClose" class="door-actions">
            <button class="ghost danger" type="button" @click="confirmClose = true">关门</button>
          </div>
          <div v-else class="confirm">
            <p class="confirm-text">关门后，门里的人都会断开，这门牌号就作废了。</p>
            <div class="confirm-actions">
              <button class="ghost danger-strong" type="button" @click="closeDoor">确定关门</button>
              <button class="ghost" type="button" @click="confirmClose = false">算了</button>
            </div>
          </div>
        </template>

        <template v-else>
          <h1 class="title">正在开门…</h1>
          <p class="lede">马上就好。</p>
        </template>
      </template>

      <!-- 房客：敲门中 -->
      <template v-else-if="state === 'punching'">
        <div class="pulse" aria-hidden="true"></div>
        <h1 class="title">正在敲门…</h1>
        <p class="lede">在找一条能过去的路。</p>
      </template>

      <!-- 门开了 -->
      <template v-else-if="state === 'direct' || state === 'relay'">
        <div class="badge" :class="state" aria-hidden="true">
          <span class="badge-dot"></span>{{ state === "relay" ? "绕了点路" : "直连" }}
        </div>
        <h1 class="title">门开了</h1>
        <p class="lede">{{ status.hint }}</p>
        <button class="ghost" type="button" @click="retry">回到门口</button>
      </template>

      <!-- 没人应门 -->
      <template v-else-if="state === 'failed'">
        <div class="badge err" aria-hidden="true"><span class="badge-dot"></span>没敲开</div>
        <h1 class="title">{{ status.label }}</h1>
        <p class="lede">{{ status.hint }}</p>
        <button class="primary" type="button" @click="retry">再试一次</button>
      </template>
      </div>
      </Transition>
    </main>

    <footer class="foot">
      <div class="foot-left">
        <button class="icon-btn" type="button" :title="'外观：' + themeLabel" :aria-label="'外观：' + themeLabel" @click="nextTheme">
          <svg v-if="theme === 'auto'" viewBox="0 0 24 24" aria-hidden="true">
            <rect x="3" y="4" width="18" height="12" rx="2" />
            <path d="M8 20h8" />
          </svg>
          <svg v-else-if="theme === 'light'" viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="12" cy="12" r="4" />
            <path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M18.4 5.6L17 7M7 17l-1.4 1.4" />
          </svg>
          <svg v-else viewBox="0 0 24 24" aria-hidden="true">
            <path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z" />
          </svg>
        </button>
        <button class="icon-btn" type="button" @click="showSettings = !showSettings" aria-label="设置" title="设置">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.6 1.6 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.6 1.6 0 0 0-1.8-.3 1.6 1.6 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.6 1.6 0 0 0-1-1.5 1.6 1.6 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.6 1.6 0 0 0 .3-1.8 1.6 1.6 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.6 1.6 0 0 0 1.5-1 1.6 1.6 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.6 1.6 0 0 0 1.8.3H9a1.6 1.6 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.6 1.6 0 0 0 1 1.5 1.6 1.6 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.6 1.6 0 0 0-.3 1.8V9a1.6 1.6 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.6 1.6 0 0 0-1.5 1z" />
          </svg>
        </button>
      </div>
      <span class="note inline">{{ state === "waiting" ? "关掉窗口门还开着" : "" }}</span>
      <span class="mono version">v0.1</span>
    </footer>
  </div>
</template>

<style scoped>
.shell {
  user-select: none;
  -webkit-user-select: none;
  min-height: 100%;
  max-width: 460px;
  margin: 0 auto;
  padding: 22px 22px 16px;
  display: flex;
  flex-direction: column;
  gap: 18px;
}

/* —— 顶栏 —— */
.bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  /* 自绘标题栏：这一整条都能拖动窗口 */
  gap: 10px;
  user-select: none;
  -webkit-user-select: none;
}
.bar .mark {
  pointer-events: none; /* 拖动优先，别让文字吃掉手势 */
}
.win-btns {
  display: flex;
  gap: 2px;
  margin-left: -4px;
}
.win-btn {
  width: 32px;
  height: 26px;
  display: grid;
  place-items: center;
  color: var(--muted);
  background: transparent;
  border: none;
  border-radius: var(--r-sm);
  cursor: pointer;
  transition: background 0.12s ease, color 0.12s ease;
}
.win-btn svg {
  width: 13px;
  height: 13px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.3;
  stroke-linecap: round;
}
.win-btn:hover {
  color: var(--text);
  background: var(--surface-2);
}
.win-btn.close:hover {
  color: #fff;
  background: #c0392b;
}
.mark {
  display: flex;
  align-items: center;
  gap: 7px;
  font-size: 13px;
}
.dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-soft);
}
.word {
  font-weight: 600;
  letter-spacing: 0.01em;
}
.sep {
  color: var(--muted);
}
.sub {
  color: var(--muted);
}

/* —— 舞台卡片：0.5px 观感靠 alpha 边框，不用纯黑 —— */
.stage {
  flex: 1;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg);
  box-shadow: var(--shadow-card);
  padding: 36px 30px 32px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  text-align: center;
  gap: 12px;
}

.title {
  margin: 0;
  font-size: 23px;
  font-weight: 600;
  letter-spacing: -0.01em;
}
.lede {
  color: var(--text-soft);
  font-size: 13.5px;
  line-height: 1.6;
  /* 居中：限宽之后必须自己居中，否则限宽框贴着左边，看起来就是歪的 */
  max-width: 26ch;
  margin: 0 auto;
  text-align: center;
  text-wrap: balance;
}
.hint {
  margin: 2px 0 0;
  color: var(--muted);
  font-size: 12.5px;
}
.eyebrow {
  margin: 0;
  font-size: 12px;
  color: var(--muted);
  letter-spacing: 0.08em;
}

/* —— 门牌号 —— */
.code-display {
  font-size: 40px;
  font-weight: 600;
  letter-spacing: 0.14em;
  padding-left: 0.14em; /* 抵消末字符的字距，视觉才居中 */
  color: var(--text);
}
.row {
  display: flex;
  align-items: center;
  gap: 10px;
}

.field {
  width: 100%;
  max-width: 300px;
  text-align: left;
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-top: 6px;
}
.field-label {
  font-size: 12px;
  color: var(--muted);
}
input,
textarea {
  user-select: text;
  -webkit-user-select: text;
}

.code-input {
  width: 100%;
  padding: 13px 14px;
  font-size: 21px;
  letter-spacing: 0.24em;
  text-align: center;
  color: var(--text);
  background: var(--surface-2);
  border: 1px solid var(--line-strong);
  border-radius: var(--r-md);
  outline: none;
  transition: border-color 0.15s ease, box-shadow 0.15s ease;
}
.code-input::placeholder {
  color: var(--muted);
  opacity: 0.55;
  letter-spacing: 0.24em;
}
.code-input:focus {
  border-color: var(--accent-line);
  box-shadow: 0 0 0 3px var(--accent-soft);
}
.small-input {
  width: 96px;
  padding: 9px 12px;
  font-size: 14px;
  color: var(--text);
  background: var(--surface-2);
  border: 1px solid var(--line-strong);
  border-radius: var(--r-sm);
  outline: none;
}
.port-input {
  letter-spacing: 0.14em;
  font-size: 19px;
  padding: 12px 14px;
}
.port-input:focus {
  border-color: var(--accent-line);
  box-shadow: 0 0 0 3px var(--accent-soft);
}

/* —— 按钮：一个主操作，其余次级 —— */
.primary {
  margin-top: 8px;
  min-width: 168px;
  padding: 11px 20px;
  font-size: 14px;
  font-weight: 550;
  color: var(--on-accent);
  background: var(--accent);
  border: 1px solid var(--accent-line);
  border-radius: var(--r-md);
  box-shadow: var(--shadow-primary);
  cursor: pointer;
  transition: transform 0.12s ease, filter 0.15s ease;
}
.primary:hover:not(:disabled) {
  filter: brightness(1.05);
}
.primary:active:not(:disabled) {
  transform: translateY(0.5px);
}
.primary:disabled {
  cursor: not-allowed;
  color: var(--muted);
  background: var(--surface-2);
  border-color: var(--line-strong);
  box-shadow: none;
}
.ghost {
  padding: 9px 16px;
  font-size: 13px;
  color: var(--text-soft);
  background: transparent;
  border: 1px solid var(--line-strong);
  border-radius: var(--r-md);
  cursor: pointer;
  transition: background 0.15s ease, color 0.15s ease;
}
.ghost:hover {
  background: var(--surface-2);
  color: var(--text);
}
.ghost.tiny {
  padding: 5px 10px;
  font-size: 12px;
  border-radius: var(--r-sm);
}

/* —— 开门面板 —— */
.panel {
  width: 100%;
  max-width: 300px;
  margin-top: 4px;
  padding: 16px;
  gap: 10px;
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  background: var(--surface-2);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
}
.panel .primary {
  width: 100%;
  min-width: 0;
}

/* —— 状态徽标：浅底深字 —— */
.badge {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 4px 11px;
  font-size: 12px;
  border-radius: 999px;
  border: 1px solid var(--line);
  color: var(--text-soft);
  background: var(--surface-2);
}
.badge.direct {
  color: var(--ok);
  background: var(--ok-soft);
  border-color: transparent;
}
.badge.relay {
  color: var(--warn);
  background: var(--warn-soft);
  border-color: transparent;
}
.badge.err {
  color: var(--err);
  background: var(--err-soft);
  border-color: transparent;
}
.badge-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
}

/* —— 等待时的呼吸点：不是转圈，避免"进度条骗人" —— */
.pulse {
  width: 34px;
  height: 34px;
  border-radius: 50%;
  background: radial-gradient(circle, var(--accent-soft) 0%, transparent 70%);
  border: 1px solid var(--accent-line);
  animation: breathe 1.8s ease-in-out infinite;
}
@keyframes breathe {
  0%, 100% { transform: scale(0.92); opacity: 0.7; }
  50% { transform: scale(1.06); opacity: 1; }
}
@media (prefers-reduced-motion: reduce) {
  .pulse { animation: none; }
}

.note {
  margin: 0;
  color: var(--muted);
  font-size: 12px;
}
.note.inline {
  white-space: nowrap;
}

/* —— 通用文本输入 —— */
.text-input {
  width: 100%;
  padding: 11px 13px;
  font-size: 14px;
  color: var(--text);
  background: var(--surface-2);
  border: 1px solid var(--line-strong);
  border-radius: var(--r-md);
  outline: none;
}
.text-input:focus {
  border-color: var(--accent-line);
  box-shadow: 0 0 0 3px var(--accent-soft);
}

.bar-right {
  display: flex;
  gap: 6px;
}

/* —— 首页的两个选择 —— */
.choice {
  width: 100%;
}
.choice-row {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
  margin-top: 16px;
}
.choice-card {
  display: flex;
  flex-direction: column;
  padding: 13px 14px;
  text-align: left;
  background: var(--surface-2);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  cursor: pointer;
  /* 苹果那套：快出慢落，按下略缩 */
  transition: transform 0.22s cubic-bezier(0.32, 0.72, 0, 1), border-color 0.2s ease,
    background 0.2s ease, box-shadow 0.24s ease;
}
.choice-card:hover {
  border-color: var(--line-strong);
  transform: translateY(-1px);
  box-shadow: 0 6px 16px -12px rgba(0, 0, 0, 0.5);
}
.choice-card:active {
  transform: scale(0.98);
}
.choice-card.on {
  border-color: var(--accent-line);
  background: var(--surface);
  box-shadow: inset 0 0 0 1px var(--accent-line), 0 0 0 3px var(--accent-soft);
}
.choice-name {
  font-size: 14px;
  font-weight: 600;
  color: var(--text);
}
.choice-sub {
  margin-top: 4px;
  font-size: 11.5px;
  line-height: 1.45;
  color: var(--muted);
}

/* 展开区：用 grid-template-rows 0fr→1fr 做高度动画，不写死 max-height */
.expander {
  width: 100%;
  display: grid;
  grid-template-rows: 0fr;
  opacity: 0;
  transition: grid-template-rows 0.42s cubic-bezier(0.32, 0.72, 0, 1), opacity 0.3s ease;
}
.expander.open {
  grid-template-rows: 1fr;
  opacity: 1;
}
.expander-inner {
  overflow: hidden;
}
.panel-inline {
  width: 100%;
  /* 不设 max-width：要和上面两张卡片同宽，否则上下两条边对不齐 */
  margin: 16px 0 2px;
  display: flex;
  flex-direction: column;
  align-items: stretch;
  gap: 10px;
}
.panel-inline .field {
  max-width: none;
}

.warn {
  margin: 10px 0 0;
  padding: 8px 12px;
  font-size: 12.5px;
  line-height: 1.5;
  text-align: center;
  color: var(--danger, #d2603a);
  background: color-mix(in oklab, var(--danger, #d2603a) 12%, transparent);
  border: 1px solid color-mix(in oklab, var(--danger, #d2603a) 28%, transparent);
  border-radius: var(--r-sm);
}
.row-setting.wide {
  grid-template-columns: 58px 1fr;
}
.setting-label {
  white-space: nowrap;
}
.small-input.grow {
  width: 100%;
  font-size: 12.5px;
  letter-spacing: 0;
}

.foot-left {
  display: flex;
  gap: 6px;
}

/* —— 门开着多久 / 关门 —— */
.open-meta {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  color: var(--text-soft);
}
.live-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--ok);
  box-shadow: 0 0 0 3px var(--ok-soft);
  animation: breathe 2.4s ease-in-out infinite;
}
.dot-sep {
  color: var(--muted);
}
.door-actions {
  margin-top: 6px;
}
.ghost.danger:hover {
  color: var(--err);
  border-color: var(--err);
  background: var(--err-soft);
}
.ghost.danger-strong {
  color: var(--err);
  border-color: var(--err);
  background: var(--err-soft);
}
.confirm {
  width: 100%;
  max-width: 320px;
  margin-top: 8px;
  padding: 12px 14px;
  background: var(--surface-2);
  border: 1px solid var(--line-strong);
  border-radius: var(--r-md);
  text-align: left;
}
.confirm-text {
  margin: 0 0 10px;
  font-size: 12.5px;
  color: var(--text-soft);
  line-height: 1.5;
}
.confirm-actions {
  display: flex;
  gap: 8px;
}

/* —— 门里的人 —— */
.guests {
  width: 100%;
  max-width: 340px;
  margin-top: 14px;
  padding-top: 12px;
  border-top: 1px solid var(--line);
  text-align: left;
}
.guests-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 12px;
  color: var(--muted);
  margin-bottom: 8px;
}
.count {
  font-size: 11px;
  padding: 1px 7px;
  border-radius: 999px;
  background: var(--surface-2);
  border: 1px solid var(--line);
}
.guest-list {
  list-style: none;
  margin: 0;
  padding: 0 0 2px;
  display: flex;
  flex-direction: column;
  gap: 2px;
  /* 人多了不能把界面撑开：最多露出四行，剩下的在里面滚 */
  max-height: 156px;
  overflow-y: auto;
  overscroll-behavior: contain;
  scrollbar-width: thin;
  scrollbar-color: var(--line-strong) transparent;
  /* 底部渐隐，提示"下面还有人" */
  mask-image: linear-gradient(to bottom, #000 0, #000 calc(100% - 16px), transparent 100%);
}
.guest-list::-webkit-scrollbar {
  width: 6px;
}
.more-note {
  margin: 6px 0 0;
  font-size: 11.5px;
  color: var(--muted);
  text-align: center;
}
.guest-list::-webkit-scrollbar-thumb {
  background: var(--line-strong);
  border-radius: 999px;
}
.guest {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 74px 62px auto;
  align-items: center;
  gap: 8px;
  padding: 7px 8px;
  border-radius: var(--r-sm);
  font-size: 13px;
}
.guest:hover {
  background: var(--surface-2);
}
.guest-name {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.guest-since {
  color: var(--muted);
  font-size: 11.5px;
  text-align: right;
}
.guest .badge {
  justify-self: start;
}
.badge.tiny {
  padding: 2px 7px;
  font-size: 11px;
}


/* —— 设置 —— */
.settings {
  width: 100%;
  max-width: 320px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  margin: 6px 0 4px;
  text-align: left;
}
.row-setting {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 12px;
  background: var(--surface-2);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
}
.setting-label {
  font-size: 13px;
  color: var(--text-soft);
}
.seg {
  display: flex;
  gap: 2px;
  padding: 2px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 999px;
}
.seg-btn {
  padding: 4px 10px;
  font-size: 12px;
  color: var(--muted);
  background: transparent;
  border: none;
  border-radius: 999px;
  cursor: pointer;
}
.seg-btn.on {
  color: var(--text);
  background: var(--accent-soft);
}

/* —— 图标按钮 —— */
.icon-btn {
  width: 30px;
  height: 30px;
  display: grid;
  place-items: center;
  color: var(--text-soft);
  background: transparent;
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  cursor: pointer;
  transition: color 0.15s ease, background 0.15s ease, border-color 0.15s ease, transform 0.12s ease;
}
.icon-btn svg {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.icon-btn:hover {
  color: var(--text);
  background: var(--surface-2);
  border-color: var(--line-strong);
}
.icon-btn:active {
  transform: scale(0.94);
}

/* —— 视图容器与动效 —— */
.view {
  width: 100%;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  text-align: center;
  gap: 12px;
}
/* 缓动取参考站的招牌曲线：出得快、落得稳 */
.view-enter-active {
  transition: opacity 0.24s cubic-bezier(0.32, 0.72, 0, 1),
    transform 0.34s cubic-bezier(0.32, 0.72, 0, 1), filter 0.3s cubic-bezier(0.32, 0.72, 0, 1);
}
.view-leave-active {
  transition: opacity 0.15s cubic-bezier(0.4, 0, 0.6, 1), transform 0.15s cubic-bezier(0.4, 0, 0.6, 1),
    filter 0.15s cubic-bezier(0.4, 0, 0.6, 1);
}
/* 横向位移 + 轻微模糊：比纯淡入更有"翻页"的实体感 */
.view-enter-from {
  opacity: 0;
  transform: translateX(14px) scale(0.994);
  filter: blur(3px);
}
.view-leave-to {
  opacity: 0;
  transform: translateX(-10px) scale(0.996);
  filter: blur(2px);
}

/* 新内容逐个浮现：比整块一起弹更"有呼吸" */
.view > * {
  animation: rise 0.34s cubic-bezier(0.22, 0.68, 0, 1) both;
}
.view > *:nth-child(1) { animation-delay: 0.02s; }
.view > *:nth-child(2) { animation-delay: 0.06s; }
.view > *:nth-child(3) { animation-delay: 0.1s; }
.view > *:nth-child(4) { animation-delay: 0.14s; }
.view > *:nth-child(5) { animation-delay: 0.18s; }
.view > *:nth-child(6) { animation-delay: 0.22s; }
@keyframes rise {
  from { opacity: 0; transform: translateY(7px); }
  to { opacity: 1; transform: translateY(0); }
}

/* 门牌号落下来：字距略收，像数字找到了位置 */
.code-display {
  animation: settle 0.44s cubic-bezier(0.22, 0.68, 0, 1) both;
}
@keyframes settle {
  from { opacity: 0; transform: translateY(6px); letter-spacing: 0.24em; }
  to { opacity: 1; transform: translateY(0); letter-spacing: 0.14em; }
}

/* 来人列表：进出都不突兀 */
.guest-enter-active,
.guest-leave-active {
  transition: opacity 0.22s cubic-bezier(0.22, 0.68, 0, 1),
    transform 0.22s cubic-bezier(0.22, 0.68, 0, 1);
}
.guest-enter-from {
  opacity: 0;
  transform: translateX(-6px);
}
.guest-leave-to {
  opacity: 0;
  transform: translateX(8px);
}
.guest-move {
  transition: transform 0.22s cubic-bezier(0.22, 0.68, 0, 1);
}

/* 请出去：是个按钮，不是一个词 */
.kick {
  padding: 3px 10px;
  font-size: 11.5px;
  line-height: 1.4;
  color: var(--muted);
  background: var(--surface);
  border: 1px solid var(--line-strong);
  border-radius: 999px;
  cursor: pointer;
  opacity: 0.75;
  transition: opacity 0.14s ease, color 0.14s ease, border-color 0.14s ease,
    background 0.14s ease, transform 0.1s ease;
}
.kick:hover {
  opacity: 1;
  color: var(--err);
  border-color: var(--err);
  background: var(--err-soft);
}
.kick:active {
  transform: scale(0.95);
}

@media (prefers-reduced-motion: reduce) {
  .view > *,
  .code-display {
    animation: none;
  }
  .view-enter-active,
  .view-leave-active,
  .guest-enter-active,
  .guest-leave-active,
  .guest-move {
    transition: none;
  }
}

/* —— 页脚 —— */
.foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  color: var(--muted);
  font-size: 11.5px;
  padding: 0 2px;
}
.version {
  opacity: 0.7;
}
</style>
