"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { Link } from "react-router-dom";
import { useNavigate } from "react-router-dom";

import { BrandLoader } from "@/components/brand-loader";
import { ContentEmptyState } from "@/components/content-empty-state";
import { useConfirm, useToast } from "@/components/feedback";
import { ChevronDownIcon, PlusIcon, SearchIcon, XIcon } from "@/components/icons";
import { LibraryFormDialog } from "@/components/library-form-dialog";
import { LibraryCoverModal } from "@/components/library-cover-modal";
import {
  type LibraryRowActions,
  type LibraryRowDrag,
  LibraryManageRow,
} from "@/components/library-manage-row";
import { Modal } from "@/components/modal";
import { LibraryRemovalNotice } from "@/components/library-removal-notice";
import { useLibraryRemoval } from "@/lib/use-library-removal";
import { PageNav } from "@/components/page-nav";
import {
  type DuplicateEntryGroup,
  previewLibraryOrganize,
  previewPathReconciliation,
  applyPathReconciliations,
  previewRootConsolidation,
  applyRootConsolidations,
  startLibraryOrganize,
  type MediaLibrary,
  deleteDuplicateFile,
  listLibraries,
  listLibraryDuplicates,
  reorderLibraries,
  setDefaultLibrary,
  startLibraryMetadataRefresh,
  startLibraryScan,
} from "@/lib/api/libraries";
import { refreshLibraryConfirm, scanLibraryConfirm } from "@/lib/library-confirm";
import { routingOverlapWarnings } from "@/lib/library-routing-warnings";
import {
  EMPTY_FILTER,
  type LibraryFilter,
  type LibraryFocus,
  filterIsActive,
  filterLibraries,
  moveInList,
  summarizeLibraries,
} from "@/lib/library-manage";
import { LIBRARY_KIND_LABELS, type LibraryKind } from "@/lib/media-types";
import { usePermissions } from "@/lib/permissions";
import { useIsMobile } from "@/lib/use-media-query";
import { useVisiblePolling } from "@/lib/use-visible-polling";

const KIND_ORDER: LibraryKind[] = ["movie", "tv", "video"];

/** 指针落在目标行的上半还是下半：决定放到它之前还是之后。 */
function dropPosition(e: React.DragEvent): "before" | "after" {
  const rect = e.currentTarget.getBoundingClientRect();
  return e.clientY < rect.top + rect.height / 2 ? "before" : "after";
}

/**
 * 媒体库管理页（/library/manage）：一库一行的纵向列表，库多了只是变长。
 *
 * 页面回答的第一个问题是「有没有事要我管」：页头摘要里只挂两枚带色胶囊——
 * 在跑任务、有待处理文件——点即筛选；两样都没有就写「一切正常」。列表行只有
 * 两个视觉重心（库名、状态），其余信息是库名下的小字。
 *
 * 首页（/library）只做浏览入口；建库、编辑、扫描、整理、刷新、设默认、
 * 首页展示开关、排序、删除全部在这里完成。设计见 docs/design/library-manage.md。
 *
 * 数据只用 listLibraries 一个接口（随库下发的统计快照与任务进度足够填满
 * 状态列），不逐库拉条目——首页为了封面拼图才要拉，这里的缩略图走服务端拼贴图。
 */
export function LibraryManageView() {
  const { canManageLibraries } = usePermissions();
  const navigate = useNavigate();
  const confirm = useConfirm();
  const toast = useToast();
  const isMobile = useIsMobile();

  // 页面只保留「媒体库」列表：回收站 / 重复文件 / 分享的端点与组件已随新契约
  // 一并删除（listTrashedFiles / listDuplicateFiles / listShares 返回空数据），
  // 对应标签与计数在组件适配阶段整体移除。
  const [tab, setTab] = useState<"libraries" | "duplicates">("libraries");
  const [libraries, setLibraries] = useState<MediaLibrary[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [filter, setFilter] = useState<LibraryFilter>(EMPTY_FILTER);
  // 弹窗态：新增（"new"）/ 编辑（库对象）/ 关闭(null)
  const [editing, setEditing] = useState<MediaLibrary | "new" | null>(null);
  const [coverModalLibrary, setCoverModalLibrary] = useState<MediaLibrary | null>(null);
  const [reorderOpen, setReorderOpen] = useState(false);
  // 新建成功后把新行滚进视野（纵向列表末尾可能在首屏外）
  const [revealId, setRevealId] = useState<string | null>(null);

  // 轮询乱序守卫：与首页同一套——扫描期间慢响应可能晚于下一轮到达
  const reloadSeq = useRef(0);
  const reload = useCallback(() => {
    const seq = ++reloadSeq.current;
    listLibraries(undefined, { manage: true })
      .then((libs) => {
        if (seq !== reloadSeq.current) return;
        setFailed(false);
        const snapshot = JSON.stringify(libs);
        setLibraries((prev) => (prev && JSON.stringify(prev) === snapshot ? prev : libs));
      })
      .catch(() => {
        if (seq === reloadSeq.current) setFailed(true);
      });
  }, []);

  const removal = useLibraryRemoval(libraries, (library) => {
    toast.success(`已删除媒体库「${library.name}」`);
    setLibraries((prev) => prev?.filter((entry) => entry.id !== library.id) ?? null);
    reload();
  });

  useEffect(() => {
    reload();
  }, [reload]);

  // 任务中心事件触发重拉：新契约的 JobDef 不再携带库资源索引（resources 已删），
  // 实时监控等外部任务改由下面的常规轮询发现，这里不再做事件指纹。
  // 首页空状态的「创建第一个媒体库」落到 /library/manage?create=1：进页即开建库弹窗。
  // 读 location 而不是 useSearchParams（全站惯例，免去 Suspense 边界）；读完把参数抹掉，
  // 刷新页面不会再弹
  useEffect(() => {
    if (!canManageLibraries) return;
    const params = new URLSearchParams(window.location.search);
    if (params.get("create") !== "1") return;
    setEditing("new");
    params.delete("create");
    const rest = params.toString();
    window.history.replaceState(null, "", `${window.location.pathname}${rest ? `?${rest}` : ""}`);
  }, [canManageLibraries]);

  // 新契约无扫描/整理/刷新等实时状态，空闲低频轮询兜底即可（30 秒）
  useVisiblePolling(reload, 30_000);

  useEffect(() => {
    if (revealId === null) return;
    document
      .querySelector(`[data-library-row="${revealId}"]`)
      ?.scrollIntoView({ behavior: "smooth", block: "center" });
    setRevealId(null);
  }, [revealId, libraries]);

  const warnings = useMemo(() => routingOverlapWarnings(libraries ?? []), [libraries]);
  const visible = useMemo(() => filterLibraries(libraries ?? [], filter), [libraries, filter]);
  const summary = useMemo(() => summarizeLibraries(libraries ?? []), [libraries]);
  /** 页头摘要胶囊即筛选：再点一次取消；在回收站标签上点则先切回库列表 */
  const toggleFocus = (focus: LibraryFocus) => {
    setFilter((f) => ({ ...f, focus: f.focus === focus ? null : focus }));
  };
  const kindCounts = useMemo(() => {
    const counts = new Map<LibraryKind, number>();
    for (const l of libraries ?? []) counts.set(l.kind, (counts.get(l.kind) ?? 0) + 1);
    return counts;
  }, [libraries]);

  /** 动作统一收口：成功后立刻拉一次列表（可选给一句回执），失败用 toast 报后端的
   *  中文错误——列表可能很长，用户在底部点的按钮，顶部横条根本看不见 */
  const run = useCallback(
    (action: Promise<unknown>, done?: string) => {
      void action
        .then(() => {
          reload();
          if (done) toast.success(done);
        })
        .catch((e) => toast.error((e as Error).message));
    },
    [reload, toast],
  );

  /** 提交新顺序：先乐观换位，失败回滚。全量 id 一次提交（后端接口要求） */
  const commitOrder = useCallback(
    (next: readonly MediaLibrary[]) => {
      const prev = libraries;
      setLibraries([...next]);
      void reorderLibraries(next.map((l) => l.id))
        .then(() => {
          reload();
          toast.success("顺序已更新，首页「我的媒体库」同步生效");
        })
        .catch((e) => {
          setLibraries(prev);
          toast.error((e as Error).message);
        });
    },
    [libraries, reload, toast],
  );

  const moveLibrary = useCallback(
    (libraryId: string, to: number) => {
      if (!libraries) return;
      const from = libraries.findIndex((l) => l.id === libraryId);
      const next = moveInList(libraries, from, to);
      if (next !== libraries) commitOrder(next);
    },
    [libraries, commitOrder],
  );

  const actions: LibraryRowActions = useMemo(
    () => ({
      onToggleScan: (library) => {
        // 重操作先确认；新契约无扫描进行中状态，只有「开始扫描」一个形态
        void confirm(scanLibraryConfirm(library.name)).then((ok) => {
          if (ok) run(startLibraryScan(library.id), `已开始扫描「${library.name}」`);
        });
      },
      onOpenPending: (library) => {
        navigate(`/library/${library.id}?pending=1`);
      },
      onToggleRefresh: (library) => {
        void confirm(refreshLibraryConfirm(library.name)).then((ok) => {
          if (ok) run(startLibraryMetadataRefresh(library.id));
        });
      },
      onEdit: (library) => setEditing(library),
      onSetDefault: (library) => {
        void confirm({
          title: `将「${library.name}」设为默认库？`,
          description: `设为默认后，未指定库的 ${library.kind === "movie" ? "电影" : "剧集"} 订阅和自动入库将优先归属到此库。原默认库将变为普通库。`,
          confirmLabel: "设为默认",
          tone: "default",
        }).then((ok) => {
          if (ok) {
            void setDefaultLibrary(library.id)
              .then(() => {
                toast.success(`已将「${library.name}」设为默认库`);
                reload();
              })
              .catch((e: Error) => toast.error(e.message));
          }
        });
      },
      onDelete: (library) => {
        void confirm({
          title: `删除媒体库「${library.name}」？`,
          description: "将停止该库相关任务，清除扫描记录与探测缓存。磁盘文件、NFO 和图片不会删除，观看进度、已看和收藏会保留。重新添加目录后将重新扫描。",
          confirmLabel: "删除媒体库",
          tone: "danger",
        }).then((ok) => {
          if (ok) {
            removal.remove(library);
          }
        });
      },
      onOrganize: (library) => {
        void organizeLibrary(library, toast, confirm);
      },
      onReconcile: (library) => {
        void reconcileLibrary(library, toast, confirm);
      },
      onConsolidate: (library) => {
        void consolidateLibrary(library, toast, confirm);
      },
      onCoverModal: (library: MediaLibrary) => {
        setCoverModalLibrary(library);
      },
      onReorder: isMobile ? () => setReorderOpen(true) : undefined,
    }),
    [confirm, isMobile, navigate, reload, run, toast, removal],
  );

  // —— 拖拽排序（桌面端、未筛选时）——
  const [dragId, setDragId] = useState<string | null>(null);
  // 拖到哪一行、落在它之前还是之后（按指针在行内的上下半判定）
  const [over, setOver] = useState<{ id: string; pos: "before" | "after" } | null>(null);
  const dragEnabled = !isMobile && !filterIsActive(filter) && (libraries?.length ?? 0) > 1;
  const dragFor = (library: MediaLibrary): LibraryRowDrag | null => {
    if (!dragEnabled || !libraries) return null;
    const index = libraries.findIndex((l) => l.id === library.id);
    return {
      dragging: dragId === library.id,
      over: over?.id === library.id && dragId !== library.id ? over.pos : null,
      onDragStart: (e) => {
        e.dataTransfer.effectAllowed = "move";
        e.dataTransfer.setData("text/plain", String(library.id));
        // 拖影用整行而不是那颗小把手，用户才看得出自己拖的是哪个库
        const row = (e.currentTarget as HTMLElement).closest("[data-library-row]");
        if (row instanceof HTMLElement) e.dataTransfer.setDragImage(row, 24, row.offsetHeight / 2);
        setDragId(library.id);
      },
      onDragOver: (e) => {
        if (dragId === null) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        const pos = dropPosition(e);
        if (over?.id !== library.id || over.pos !== pos) setOver({ id: library.id, pos });
      },
      onDrop: (e) => {
        e.preventDefault();
        if (dragId !== null && dragId !== library.id) {
          const from = libraries.findIndex((l) => l.id === dragId);
          const before = dropPosition(e) === "before";
          // 目标位置以「拿走被拖的那一行之后」的列表计：从上往下拖时目标行会前移一位
          const to = before ? (from < index ? index - 1 : index) : from < index ? index : index + 1;
          moveLibrary(dragId, to);
        }
        setDragId(null);
        setOver(null);
      },
      onDragEnd: () => {
        setDragId(null);
        setOver(null);
      },
      onMoveKey: (offset) => moveLibrary(library.id, index + offset),
    };
  };

  if (!canManageLibraries) {
    return (
      <div className="scroll-thin scroll-safe flex-1 overflow-y-auto pb-10">
        <PageNav title="媒体库管理" fallback={{ label: "媒体库", href: "/library" }} />
        <ContentEmptyState
          variant="library"
          title="没有管理权限"
          description="媒体库的创建、扫描与排序由管理员负责；你可以回到媒体库继续浏览。"
          action={
            <Link to={"/library"} className="btn-glass px-4 py-2 text-ui font-medium">
              返回媒体库
            </Link>
          }
        />
      </div>
    );
  }

  return (
    <div className="scroll-thin scroll-safe flex-1 overflow-y-auto pb-10">
      <PageNav title="媒体库管理" fallback={{ label: "媒体库", href: "/library" }} />

      {/* 页头：标题 + 说明，右侧是页面级动作「创建媒体库」（与首页「管理媒体库」
          同一位置约定：页面动作放标题行右端，顶栏只留返回与吸顶标题） */}
      <div className="px-6 pt-3 max-md:px-4">
        <div className="flex items-start justify-between gap-4">
          <h2 className="text-on-image text-[26px] font-bold leading-tight tracking-[-0.02em] text-white max-md:text-[21px]">
            媒体库管理
          </h2>
          <button
            type="button"
            onClick={() => setEditing("new")}
            className="btn-accent mt-1 flex h-9 shrink-0 items-center gap-1 rounded-full py-0 pl-3 pr-4 text-ui font-semibold max-md:mt-0"
          >
            <PlusIcon className="size-4" />
            创建媒体库
          </button>
        </div>
        {/* 副标题是一行活的摘要，独占一行（不与按钮争宽，手机端才不会把数字挤断）：
            规模事实之后紧跟这页真正要你看的两件事——在跑任务与待处理文件——做成带色胶囊，
            点即筛选；两样都没有就明说「一切正常」 */}
        <div className="text-on-image mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1.5 text-ui text-[var(--text-muted)] max-md:text-sub">
          <span>
            {libraries === null
              ? "正在汇总媒体库…"
              : libraries.length === 0
                ? "还没有媒体库"
                : summary.facts}
          </span>
          {summary.busy > 0 && (
            <FilterChip active={filter.focus === "busy"} onClick={() => toggleFocus("busy")}>
              <span className="size-1.5 rounded-full bg-[var(--info)]" />
              {summary.busy} 个在跑任务
            </FilterChip>
          )}
          {summary.attention > 0 && (
            <FilterChip
              active={filter.focus === "attention"}
              onClick={() => toggleFocus("attention")}
            >
              <span
                className={`size-1.5 rounded-full ${summary.missing ? "bg-[var(--danger)]" : "bg-[var(--warn)]"}`}
              />
              {summary.attention} 个库有待处理文件
            </FilterChip>
          )}
          {libraries !== null &&
            libraries.length > 0 &&
            summary.busy === 0 &&
            summary.attention === 0 && <span className="text-[var(--text-faint)]">一切正常</span>}
        </div>
      </div>

      {/* 二级视图：媒体库 / 回收站（延迟删除）/ 重复文件 */}
      <div className="mt-5 flex flex-wrap gap-1.5 px-6 max-md:px-4">
        {(
          [
            ["libraries", "媒体库"],
            ["duplicates", "重复文件"],
          ] as const
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            aria-pressed={tab === id}
            onClick={() => setTab(id)}
            className={`rounded-full px-3.5 py-1.5 text-ui font-medium transition ${
              tab === id
                ? "bg-white/[0.14] text-white"
                : "text-[var(--text-muted)] hover:bg-white/[0.06] hover:text-white"
            }`}
          >
            {label}
          </button>
        ))}
      </div>

      {tab === "libraries" && (
      <>
      {failed && libraries !== null && (
        <div className="mx-6 mt-4 rounded-xl border border-amber-400/25 bg-amber-500/10 px-4 py-3 text-sub text-amber-200 max-md:mx-4">
          与后端通信失败，正在自动重试；下方显示的是最近一次成功加载的数据
        </div>
      )}

      {libraries === null && !failed && (
        <div className="mt-16 flex items-center justify-center gap-2.5 text-ui text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在加载媒体库…
        </div>
      )}
      {failed && libraries === null && (
        <div className="mt-16 flex flex-col items-center gap-3 text-center">
          <p className="text-ui text-[var(--text-muted)]">媒体库加载失败</p>
          <button type="button" onClick={reload} className="btn-glass px-4 py-2 text-ui font-medium text-[var(--text)]">
            重试
          </button>
        </div>
      )}

      {libraries !== null && libraries.length === 0 && (
        <ContentEmptyState
          variant="library"
          title="为收藏准备一个家"
          description="创建电影库或剧集库，选好根目录后，订阅完成的内容会自动整理到这里。"
          action={
            <button
              type="button"
              onClick={() => setEditing("new")}
              className="btn-accent flex items-center gap-1 rounded-full py-2 pl-3 pr-4 text-ui font-semibold"
            >
              <PlusIcon className="size-4" />
              创建第一个媒体库
            </button>
          }
        />
      )}

      {libraries !== null && libraries.length > 0 && (
        <>
          {/* 工具栏：搜索 / 类型筛选（状态筛选在页头摘要的胶囊上） */}
          <div className="mt-5 flex flex-wrap items-center gap-2.5 px-6 max-md:px-4">
            <label className="flex h-9 min-w-[220px] flex-1 items-center gap-2 rounded-full border border-white/[0.08] bg-white/[0.04] px-3 text-ui text-[var(--text-muted)] focus-within:border-[var(--accent)]/60 max-md:min-w-0 max-md:basis-full sm:max-w-[320px]">
              <SearchIcon className="size-4 shrink-0" />
              <input
                type="search"
                value={filter.query}
                onChange={(e) => setFilter((f) => ({ ...f, query: e.target.value }))}
                placeholder="按库名或根目录搜索"
                aria-label="搜索媒体库"
                className="min-w-0 flex-1 bg-transparent text-[var(--text)] outline-none placeholder:text-[var(--text-faint)]"
              />
              {filter.query && (
                <button
                  type="button"
                  aria-label="清除搜索"
                  onClick={() => setFilter((f) => ({ ...f, query: "" }))}
                  className="grid size-5 place-items-center rounded-full hover:bg-white/[0.1]"
                >
                  <XIcon className="size-3" />
                </button>
              )}
            </label>
            <div className="flex flex-wrap items-center gap-1.5">
              <FilterChip
                active={filter.kind === null}
                onClick={() => setFilter((f) => ({ ...f, kind: null }))}
              >
                全部 {libraries.length}
              </FilterChip>
              {KIND_ORDER.filter((k) => (kindCounts.get(k) ?? 0) > 0).map((k) => (
                <FilterChip
                  key={k}
                  active={filter.kind === k}
                  onClick={() => setFilter((f) => ({ ...f, kind: f.kind === k ? null : k }))}
                >
                  {LIBRARY_KIND_LABELS[k]} {kindCounts.get(k)}
                </FilterChip>
              ))}
            </div>
          </div>

          {/* 收藏范围重叠提示：只读不阻断，原在首页，现在只在这里出现 */}
          {warnings.map((w) => (
            <div
              key={w}
              className="mx-6 mt-4 rounded-xl border border-amber-400/25 bg-amber-500/10 px-4 py-3 text-sub leading-relaxed text-amber-200 max-md:mx-4"
            >
              {w}
            </div>
          ))}

          {/* 列表：不设表头——一行只有库名 / 库存 / 状态三样，各自的形态已经说明了自己是什么，
              一条表头只会让它更像一张表。筛选空结果时给「清除筛选」 */}
          <div className="mx-6 mt-4 overflow-hidden rounded-2xl border border-white/[0.08] bg-white/[0.02] max-md:mx-4">
            {visible.length === 0 ? (
              <div className="px-4 py-10 text-center text-ui text-[var(--text-muted)]">
                没有符合条件的媒体库
                <button
                  type="button"
                  onClick={() => setFilter(EMPTY_FILTER)}
                  className="ml-2 text-[var(--info)] hover:underline"
                >
                  清除筛选
                </button>
              </div>
            ) : (
              <div role="list" aria-label="媒体库列表" className="divide-y divide-white/[0.06]">
                {visible.map((library) => (
                  <div key={library.id}>
                    <LibraryManageRow
                      library={library}
                      actions={actions}
                      removalBlocked={Boolean(removal.states[library.id]?.pending || removal.states[library.id]?.operation || library.removal)}
                      drag={removal.states[library.id]?.pending || library.removal ? null : dragFor(library)}
                    />
                    <div className="px-4">
                      <LibraryRemovalNotice library={library} state={removal.states[library.id]} onRetry={(operation) => operation ? removal.resume(library, operation) : removal.remove(library)} />
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* 底部只留一句排序提示；状态胶囊自带文字，不需要颜色图例 */}
          <p className="mx-6 mt-3 text-caption text-[var(--text-faint)] max-md:mx-4">
            {isMobile
              ? "顺序即首页「我的媒体库」的展示顺序，在 ··· 菜单里「调整顺序」"
              : dragEnabled
                ? "把指针停在行上，拖动行首的把手调整首页「我的媒体库」的展示顺序，松手即保存"
                : filterIsActive(filter)
                  ? "清除筛选后可拖拽排序"
                  : ""}
          </p>
        </>
      )}

      </>
      )}

      {tab === "duplicates" && <DuplicatesPanel />}

      <LibraryFormDialog
        state={editing}
        onClose={() => setEditing(null)}
        onSaved={(saved) => {
          const isNew = editing === "new";
          setEditing(null);
          if (isNew) setRevealId(saved.id);
          reload();
        }}
      />
      {coverModalLibrary && (
        <LibraryCoverModal
          library={coverModalLibrary}
          open={Boolean(coverModalLibrary)}
          onOpenChange={(open) => !open && setCoverModalLibrary(null)}
          onCoverUpdated={reload}
        />
      )}
      {libraries && (
        <ReorderDialog
          open={reorderOpen}
          libraries={libraries}
          onClose={() => setReorderOpen(false)}
          onSubmit={(next) => {
            setReorderOpen(false);
            commitOrder(next);
          }}
        />
      )}
    </div>
  );
}

function FilterChip({
  active,
  onClick,
  className = "",
  children,
}: {
  active: boolean;
  onClick: () => void;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      className={`flex h-7 items-center gap-1.5 rounded-full border px-2.5 text-caption font-medium transition ${
        active
          ? "border-white/[0.2] bg-white/[0.14] text-[var(--text)]"
          : "border-white/[0.1] text-[var(--text-muted)] hover:bg-white/[0.06] hover:text-[var(--text)]"
      } ${className}`}
    >
      {children}
    </button>
  );
}

/**
 * 手机端的排序弹窗：没有拖拽，上下箭头换位，确认后一次提交整单。
 * 弹窗内部持有一份顺序草稿，取消不影响列表。
 */
function ReorderDialog({
  open,
  libraries,
  onClose,
  onSubmit,
}: {
  open: boolean;
  libraries: MediaLibrary[];
  onClose: () => void;
  onSubmit: (next: readonly MediaLibrary[]) => void;
}) {
  const [draft, setDraft] = useState<readonly MediaLibrary[]>(libraries);
  useEffect(() => {
    if (open) setDraft(libraries);
  }, [open, libraries]);
  const changed = draft.some((l, i) => l.id !== libraries[i]?.id);
  return (
    <Modal open={open} onClose={onClose} label="调整媒体库顺序">
      <div className="p-5">
        <h2 className="text-title font-bold text-white">调整顺序</h2>
        <p className="mt-1 text-sub text-[var(--text-muted)]">这也是首页「我的媒体库」的展示顺序。</p>
        <ol className="mt-4 divide-y divide-white/[0.06] overflow-hidden rounded-xl border border-white/[0.08]">
          {draft.map((library, index) => (
            <li key={library.id} className="flex items-center gap-3 px-3 py-2.5">
              <span className="w-5 text-caption tabular-nums text-[var(--text-faint)]">{index + 1}</span>
              <span className="min-w-0 flex-1 truncate text-ui font-medium">{library.name}</span>
              <button
                type="button"
                aria-label={`「${library.name}」上移`}
                disabled={index === 0}
                onClick={() => setDraft((d) => moveInList(d, index, index - 1))}
                className="grid size-8 place-items-center rounded-full border border-white/[0.09] text-white/75 disabled:opacity-30"
              >
                <ChevronDownIcon className="size-4 rotate-180" />
              </button>
              <button
                type="button"
                aria-label={`「${library.name}」下移`}
                disabled={index === draft.length - 1}
                onClick={() => setDraft((d) => moveInList(d, index, index + 1))}
                className="grid size-8 place-items-center rounded-full border border-white/[0.09] text-white/75 disabled:opacity-30"
              >
                <ChevronDownIcon className="size-4" />
              </button>
            </li>
          ))}
        </ol>
        <div className="mt-5 flex justify-end gap-2">
          <button type="button" onClick={onClose} className="btn-glass px-4 py-2 text-ui font-medium">
            取消
          </button>
          <button
            type="button"
            disabled={!changed}
            onClick={() => onSubmit(draft)}
            className="btn-accent rounded-full px-4 py-2 text-ui font-semibold disabled:opacity-40"
          >
            保存顺序
          </button>
        </div>
      </div>
    </Modal>
  );
}

/**
 * 整理文件名：预览按命名模板算出的目标名，确认后应用。
 */
async function organizeLibrary(
  library: MediaLibrary,
  toast: ReturnType<typeof useToast>,
  confirm: ReturnType<typeof useConfirm>,
) {
  try {
    const preview = await previewLibraryOrganize(library.id);
    if (preview.renames.length === 0) {
      toast.success(
        preview.already_ok > 0 ? `「${library.name}」文件名已符合模板` : `「${library.name}」没有可整理的文件`,
      );
      return;
    }
    const ok = await confirm({
      title: `整理「${library.name}」的 ${preview.renames.length} 个文件？`,
      description: "按命名模板重命名；原有路径会同步到台账与订阅记录。",
      confirmLabel: "开始整理",
    });
    if (!ok) return;
    const result = await startLibraryOrganize(
      library.id,
      preview.renames.map((r) => ({ from: r.from, to: r.to })),
    );
    toast.success(`已整理 ${result.applied} 个文件${result.errors.length > 0 ? `，${result.errors.length} 个失败` : ""}`);
  } catch (error) {
    toast.error(error instanceof Error ? error.message : "整理失败");
  }
}

/** 路径核对：台账失联文件 → 库内同名候选 → 确认重关联。 */
async function reconcileLibrary(
  library: MediaLibrary,
  toast: ReturnType<typeof useToast>,
  confirm: ReturnType<typeof useConfirm>,
) {
  try {
    const { missing } = await previewPathReconciliation(library.id);
    if (missing.length === 0) {
      toast.success(`「${library.name}」台账文件全部在位`);
      return;
    }
    const auto = missing
      .map((row) => ({ from: row.path, to: row.candidates[0] ?? null }))
      .filter((x): x is { from: string; to: string } => x.to != null);
    if (auto.length === 0) {
      toast.error(`有 ${missing.length} 个失联文件，但库内没找到同名候选`);
      return;
    }
    const ok = await confirm({
      title: `重新关联 ${auto.length} 个失联文件？`,
      description: `${missing.length} 个文件不在原位；将按同名候选把台账指向新位置。`,
      confirmLabel: "重新关联",
    });
    if (!ok) return;
    const result = await applyPathReconciliations(library.id, auto);
    toast.success(`已重新关联 ${result.reconciled} 个文件${result.errors.length ? `，${result.errors.length} 个失败` : ""}`);
  } catch (error) {
    toast.error(error instanceof Error ? error.message : "路径核对失败");
  }
}

/** 根目录归并：多个根指向同一物理目录时删重复根。 */
async function consolidateLibrary(
  library: MediaLibrary,
  toast: ReturnType<typeof useToast>,
  confirm: ReturnType<typeof useConfirm>,
) {
  try {
    const { duplicates } = await previewRootConsolidation(library.id);
    if (duplicates.length === 0) {
      toast.success(`「${library.name}」没有重复的根目录`);
      return;
    }
    const mergeRoots = duplicates.map((d) => d.merge);
    const ok = await confirm({
      title: `归并 ${mergeRoots.length} 个重复根目录？`,
      description: duplicates.map((d) => `${d.merge} → 保留 ${d.keep}`).join("；"),
      confirmLabel: "归并",
    });
    if (!ok) return;
    const result = await applyRootConsolidations(library.id, mergeRoots);
    toast.success(`已归并 ${result.consolidated} 个根目录`);
  } catch (error) {
    toast.error(error instanceof Error ? error.message : "根目录归并失败");
  }
}

/**
 * 重复文件：同一 (media, season, episode) 单元多份文件，选择保留者，
 * 其余直接物理删除。
 */
function DuplicatesPanel() {
  const toast = useToast();
  const confirm = useConfirm();
  const [libraries, setLibraries] = useState<MediaLibrary[]>([]);
  const [libraryId, setLibraryId] = useState("");
  const [groups, setGroups] = useState<DuplicateEntryGroup[] | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void listLibraries()
      .then((libs) => {
        setLibraries(libs);
        if (libs.length > 0) setLibraryId(libs[0].id);
      })
      .catch(() => undefined);
  }, []);

  const reload = useCallback(() => {
    if (!libraryId) return;
    void listLibraryDuplicates(libraryId)
      .then(setGroups)
      .catch((error) => toast.error(`读取重复文件失败：${(error as Error).message}`));
  }, [libraryId, toast]);

  useEffect(() => {
    reload();
  }, [reload]);

  const remove = async (fileId: string, title: string) => {
    if (busy) return;
    const ok = await confirm({
      title: "删除这份重复文件？",
      description: "该文件将直接从磁盘物理删除，无法通过回收站恢复；做种原盘文件不受影响。",
      confirmLabel: "永久删除",
      tone: "danger",
    });
    if (!ok) return;
    setBusy(true);
    try {
      await deleteDuplicateFile(libraryId, fileId);
      toast.success(`「${title}」已删除`);
      reload();
    } catch (error) {
      toast.error((error as Error).message || "操作失败");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="px-6 pt-4 max-md:px-4">
      <label className="mb-2 block text-sub font-medium text-[var(--text-muted)]">选择媒体库</label>
      <select
        value={libraryId}
        onChange={(e) => setLibraryId(e.target.value)}
        className="w-full max-w-sm rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
      >
        {libraries.map((lib) => (
          <option key={lib.id} value={lib.id}>
            {lib.name}
          </option>
        ))}
      </select>

      {groups === null ? (
        <div className="flex items-center justify-center gap-2.5 py-14 text-ui text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在扫描重复文件…
        </div>
      ) : groups.length === 0 ? (
        <p className="py-10 text-center text-ui text-[var(--text-muted)]">这个库里没有重复文件</p>
      ) : (
        <div className="mt-3 space-y-3">
          {groups.map((group) => (
            <div key={group.media_item_id} className="rounded-2xl border border-white/[0.08] bg-white/[0.02] p-4">
              <p className="text-ui font-semibold text-white/90">
                {group.title}
                {group.season != null && (
                  <span className="tnum ml-2 text-caption text-white/50">
                    S{String(group.season).padStart(2, "0")}E{String(group.episode).padStart(2, "0")}
                  </span>
                )}
              </p>
              <div className="mt-2 space-y-1.5">
                {group.files.map((file) => (
                  <div key={file.file_id} className="flex items-center gap-3">
                    <div className="min-w-0 flex-1">
                      <p className="truncate font-mono text-caption text-white/60" title={file.path}>
                        {file.path}
                      </p>
                      <p className="text-caption text-white/35">
                        {[file.resolution, file.codec?.toUpperCase(), file.hdr]
                          .filter(Boolean)
                          .join(" · ") || "未知规格"}
                        {" · "}
                        {formatManageBytes(file.size_bytes)}
                      </p>
                    </div>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => void remove(file.file_id, group.title)}
                      className="shrink-0 rounded-lg border border-red-400/25 px-3 py-1.5 text-caption font-medium text-red-300 transition hover:bg-red-500/10 disabled:opacity-40"
                    >
                      删除此版本
                    </button>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function formatManageBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 || unit === 0 ? 0 : 1)} ${units[unit]}`;
}
