<script lang="ts">
  import {
    FileJson2,
    FileText,
    FileType2,
    FolderClosed,
    Search,
    X,
  } from '@lucide/svelte';
  import { readMarkdownFile } from '../../lib/desktop/tauriStorage';
  import type { RecentEntry } from '../../lib/desktop/tauriStorage';
  import type { Tab } from '../types';
  import { t } from '../i18n';

  export let tabs: Tab[];
  export let activeTabId: string;
  export let recentFiles: RecentEntry[];
  export let open = false;
  export let showTrigger = false;
  export let switchTab: (tabId: string) => void;
  export let openRecentEntry: (path: string) => void;
  export let removeRecentEntry: (path: string) => void;

  let visible = false;
  let query = '';
  let contentIndex = new Map<string, string>();
  let indexingPaths = new Set<string>();
  let indexRunActive = false;
  let drawerElement: HTMLElement | null = null;
  let edgeElement: HTMLElement | null = null;
  let pointerId: number | null = null;
  let pointerStartX = 0;
  let pointerStartY = 0;
  let pointerLastX = 0;
  let pointerLastTime = 0;
  let dragOffset = 0;
  let dragAxis: 'undecided' | 'horizontal' | 'vertical' = 'undecided';
  let isDragging = false;

  interface CachedDocument {
    key: string;
    id?: string;
    path: string;
    name: string;
    directory: string;
    dirty: boolean;
    active: boolean;
    openedAt: number;
    kind: Tab['documentKind'];
    content: string;
  }

  function normalizeDirectory(path: string) {
    if (!path) return t.currentFolder();
    const normalized = path.replace(/\\/g, '/');
    const index = normalized.lastIndexOf('/');
    return index > 0 ? normalized.slice(0, index) : normalized;
  }

  function normalizeSearch(value: string) {
    return value.trim().toLowerCase().normalize('NFKD');
  }

  function fuzzyMatch(haystack: string, needle: string) {
    let cursor = 0;
    for (const character of needle) {
      cursor = haystack.indexOf(character, cursor);
      if (cursor < 0) return false;
      cursor += 1;
    }
    return true;
  }

  $: recentByPath = new Map(
    recentFiles
      .filter((entry) => entry.entryType === 'file')
      .map((entry) => [entry.path.toLowerCase(), entry]),
  );

  $: documents = [
    ...tabs.map((tab): CachedDocument => ({
      key: `tab:${tab.id}`,
      id: tab.id,
      path: tab.nativePath ?? tab.filePath,
      name: tab.fileName || tab.filePath,
      directory: normalizeDirectory(tab.nativePath ?? tab.filePath),
      dirty: tab.dirty,
      active: tab.id === activeTabId,
      openedAt: recentByPath.get((tab.nativePath ?? tab.filePath).toLowerCase())?.openedAt ?? 0,
      kind: tab.documentKind,
      content: tab.documentKind === 'markdown' ? tab.markdown : '',
    })),
    ...recentFiles
      .filter(
        (entry) =>
          entry.entryType === 'file' &&
          !tabs.some(
            (tab) => (tab.nativePath ?? tab.filePath).toLowerCase() === entry.path.toLowerCase(),
          ),
      )
      .map((entry): CachedDocument => ({
        key: `recent:${entry.path}`,
        path: entry.path,
        name: entry.title || entry.path.replace(/\\/g, '/').split('/').pop() || entry.path,
        directory: normalizeDirectory(entry.path),
        dirty: false,
        active: false,
        openedAt: entry.openedAt,
        kind: entry.path.toLowerCase().endsWith('.json') ? 'json' : 'markdown',
        content: '',
      })),
  ].sort((left, right) => {
    const sourceOrder = left.directory.localeCompare(right.directory, undefined, { sensitivity: 'base' });
    return sourceOrder || right.openedAt - left.openedAt;
  });

  async function indexContent(document: CachedDocument) {
    if (contentIndex.has(document.path) || indexingPaths.has(document.path)) return;
    indexingPaths.add(document.path);
    indexingPaths = new Set(indexingPaths);
    try {
      const result = await readMarkdownFile(document.path);
      contentIndex.set(document.path, normalizeSearch(result.markdown));
    } catch {
      contentIndex.set(document.path, '');
    } finally {
      indexingPaths.delete(document.path);
      indexingPaths = new Set(indexingPaths);
      contentIndex = new Map(contentIndex);
    }
  }

  async function ensureContentIndex() {
    if (indexRunActive) return;
    const pending = documents.filter(
      (document) => !document.content && !contentIndex.has(document.path),
    );
    if (!pending.length) return;
    indexRunActive = true;
    let cursor = 0;
    const worker = async () => {
      while (cursor < pending.length) {
        const document = pending[cursor];
        cursor += 1;
        await indexContent(document);
      }
    };
    await Promise.all(Array.from({ length: Math.min(4, pending.length) }, () => worker()));
    indexRunActive = false;
  }

  $: if (open && normalizeSearch(query)) {
    void ensureContentIndex();
  }

  $: filteredDocuments = documents.filter((document) => {
    const normalizedQuery = normalizeSearch(query);
    if (!normalizedQuery) return true;
    const name = normalizeSearch(document.name);
    const content = normalizeSearch(document.content);
    return (
      name.includes(normalizedQuery) ||
      fuzzyMatch(name, normalizedQuery) ||
      content.includes(normalizedQuery) ||
      contentIndex.get(document.path)?.includes(normalizedQuery) === true
    );
  });

  $: groupedDocuments = filteredDocuments.reduce<Array<[string, CachedDocument[]]>>((groups, document) => {
    const group = groups[groups.length - 1];
    if (group?.[0] === document.directory) group[1].push(document);
    else groups.push([document.directory, [document]]);
    return groups;
  }, []);

  function iconFor(kind: CachedDocument['kind']) {
    return kind === 'json' ? FileJson2 : kind === 'text' ? FileType2 : FileText;
  }

  function clampOffset(value: number) {
    const width = drawerElement?.getBoundingClientRect().width ?? Math.min(window.innerWidth * 0.86, 360);
    return Math.max(-width, Math.min(0, value));
  }

  function setOpen(nextOpen: boolean) {
    if (nextOpen) visible = true;
    open = nextOpen;
    dragOffset = 0;
  }

  function beginDrag(event: PointerEvent, source: 'edge' | 'drawer') {
    if (event.pointerType === 'mouse' || pointerId !== null) return;
    if (source === 'edge' && open) return;
    if (source === 'drawer' && !open) return;
    if (
      source === 'drawer' &&
      event.target instanceof Element &&
      event.target.closest('input, button')
    ) {
      return;
    }
    pointerId = event.pointerId;
    pointerStartX = event.clientX;
    pointerStartY = event.clientY;
    pointerLastX = event.clientX;
    pointerLastTime = performance.now();
    dragAxis = 'undecided';
    isDragging = false;
    if (source === 'edge') {
      visible = true;
      edgeElement?.setPointerCapture(event.pointerId);
    }
  }

  function moveDrag(event: PointerEvent) {
    if (pointerId !== event.pointerId) return;
    const deltaX = event.clientX - pointerStartX;
    const deltaY = event.clientY - pointerStartY;
    if (dragAxis === 'undecided' && Math.max(Math.abs(deltaX), Math.abs(deltaY)) >= 10) {
      dragAxis = Math.abs(deltaX) > Math.abs(deltaY) ? 'horizontal' : 'vertical';
    }
    if (dragAxis !== 'horizontal') return;
    if (event.cancelable) event.preventDefault();
    isDragging = true;
    const width = drawerElement?.getBoundingClientRect().width ?? window.innerWidth * 0.86;
    dragOffset = open ? clampOffset(deltaX) : Math.min(width, Math.max(0, deltaX));
    pointerLastX = event.clientX;
    pointerLastTime = performance.now();
  }

  function endDrag(event: PointerEvent) {
    if (pointerId !== event.pointerId) return;
    const deltaX = event.clientX - pointerStartX;
    const elapsed = Math.max(1, performance.now() - pointerLastTime);
    const velocity = (event.clientX - pointerLastX) / elapsed;
    const width = drawerElement?.getBoundingClientRect().width ?? window.innerWidth * 0.86;
    const projected = (open ? width + dragOffset : dragOffset) + velocity * 180;
    const shouldOpen = open ? projected > width * 0.46 : projected > width * 0.48;
    if (dragAxis === 'horizontal' || isDragging) setOpen(shouldOpen);
    else if (!open) visible = false;
    pointerId = null;
    dragAxis = 'undecided';
    isDragging = false;
  }

  function handleDocument(document: CachedDocument) {
    setOpen(false);
    if (document.id) switchTab(document.id);
    else openRecentEntry(document.path);
  }

  function handleWindowPointerDown(event: PointerEvent) {
    if (event.pointerType === 'mouse' || pointerId !== null) return;
    if (!open) {
      if (event.clientX <= 56) beginDrag(event, 'edge');
      return;
    }
    if (drawerElement?.contains(event.target as Node)) beginDrag(event, 'drawer');
  }
</script>

<svelte:window
  on:pointerdown={handleWindowPointerDown}
  on:pointermove={moveDrag}
  on:pointerup={endDrag}
  on:pointercancel={endDrag}
/>

<div class="mobile-drawer-root" class:open aria-label={t.file()}>
  {#if !open && showTrigger}
    <button
      class="mobile-drawer-trigger"
      type="button"
      aria-label={t.file()}
      on:click={() => setOpen(true)}
    >
      <FolderClosed size={20} />
    </button>
  {/if}

  {#if !open}
    <div
      bind:this={edgeElement}
      class="mobile-drawer-edge"
      aria-hidden="true"
      on:pointerdown={(event) => beginDrag(event, 'edge')}
    ></div>
  {/if}

  {#if open}
    <button class="mobile-drawer-backdrop" type="button" aria-label={t.close()} on:click={() => setOpen(false)}></button>
  {/if}

  <aside
    bind:this={drawerElement}
    class="mobile-drawer-panel"
    class:open
    class:visible
    class:dragging={isDragging}
    aria-hidden={!open}
    style={`--drawer-offset: ${dragOffset}px`}
    on:transitionend={(event) => {
      if (event.propertyName === 'transform' && !open) visible = false;
    }}
  >
    <header class="mobile-drawer-header">
      <div class="mobile-drawer-title">
        <strong>{t.file()}</strong>
        <span>{documents.length}</span>
      </div>
      <button type="button" aria-label={t.close()} on:click={() => setOpen(false)}>
        <X size={22} />
      </button>
    </header>

    <label class="mobile-drawer-search">
      <Search size={18} />
      <input type="search" placeholder={t.searchReady()} bind:value={query} />
    </label>

    <div class="mobile-drawer-list">
      {#if query && !filteredDocuments.length}
        <p class="mobile-drawer-empty">{t.searchResultCount({ count: 0 })}</p>
      {:else if !filteredDocuments.length}
        <p class="mobile-drawer-empty">{t.currentFolder()}</p>
      {:else}
        {#each groupedDocuments as [directory, group]}
          <section class="mobile-drawer-group">
            <h2 title={directory}><FolderClosed size={15} /><span>{directory}</span></h2>
            {#each group as document}
              {@const DocumentIcon = iconFor(document.kind)}
              <div
                class="mobile-drawer-document"
                class:active={document.active}
                role="button"
                tabindex="0"
                on:click={() => handleDocument(document)}
                on:keydown={(event) => {
                  if (event.key === 'Enter' || event.key === ' ') {
                    event.preventDefault();
                    handleDocument(document);
                  }
                }}
              >
                <DocumentIcon size={18} />
                <span>{document.name}</span>
                {#if document.dirty}
                  <i class="mobile-drawer-dirty" aria-label={t.unsavedChanges()}></i>
                {:else if !document.id}
                  <button
                    class="mobile-drawer-remove"
                    type="button"
                    aria-label={t.delete()}
                    on:click|stopPropagation={() => removeRecentEntry(document.path)}
                  ><X size={14} /></button>
                {/if}
              </div>
            {/each}
          </section>
        {/each}
      {/if}
    </div>
  </aside>
</div>

<style>
  .mobile-drawer-root {
    position: fixed;
    inset: 0;
    z-index: 4000;
    pointer-events: none;
  }

  .mobile-drawer-trigger,
  .mobile-drawer-edge,
  .mobile-drawer-panel,
  .mobile-drawer-backdrop {
    pointer-events: auto;
  }

  .mobile-drawer-trigger {
    position: fixed;
    top: max(34px, env(safe-area-inset-top));
    left: max(10px, env(safe-area-inset-left));
    z-index: 4003;
    display: grid;
    width: 48px;
    height: 48px;
    place-items: center;
    border: 1px solid color-mix(in srgb, var(--md-editor-border) 72%, transparent);
    border-radius: 14px;
    background: color-mix(in srgb, var(--md-editor-surface) 94%, transparent);
    color: var(--md-editor-muted-fg);
    box-shadow: 0 8px 24px rgb(0 0 0 / 12%);
    backdrop-filter: blur(16px);
  }

  .mobile-drawer-trigger:active,
  .mobile-drawer-panel button:active {
    transform: scale(0.97);
  }

  .mobile-drawer-edge {
    position: fixed;
    top: calc(max(30px, env(safe-area-inset-top)) + 52px);
    bottom: env(safe-area-inset-bottom);
    left: 0;
    z-index: 4001;
    width: 64px;
    touch-action: pan-y;
  }

  .mobile-drawer-backdrop {
    position: fixed;
    inset: 0;
    z-index: 4001;
    border: 0;
    background: rgb(0 0 0 / 34%);
    opacity: 0;
    animation: mobile-drawer-fade-in 220ms ease-out forwards;
  }

  .mobile-drawer-panel {
    position: fixed;
    inset: max(30px, env(safe-area-inset-top)) auto env(safe-area-inset-bottom) 0;
    z-index: 4002;
    display: flex;
    width: min(86vw, 360px);
    flex-direction: column;
    padding: 12px max(12px, env(safe-area-inset-left)) 12px 12px;
    overflow: hidden;
    background: var(--md-editor-rail);
    border-right: 1px solid var(--md-editor-border);
    box-shadow: 18px 0 50px rgb(0 0 0 / 22%);
    transform: translateX(calc(-100% + var(--drawer-offset)));
    transition: transform 320ms cubic-bezier(0.2, 0, 0, 1), visibility 0s linear 320ms;
    touch-action: pan-y;
  }

  .mobile-drawer-root.open .mobile-drawer-panel {
    visibility: visible;
    pointer-events: auto;
    transform: translateX(var(--drawer-offset));
    transition: transform 320ms cubic-bezier(0.2, 0, 0, 1);
  }

  .mobile-drawer-panel:not(.open) { pointer-events: none; }

  .mobile-drawer-panel:not(.visible) {
    visibility: hidden;
    pointer-events: none;
  }

  .mobile-drawer-panel.dragging {
    transition: none;
  }

  .mobile-drawer-header,
  .mobile-drawer-title {
    display: flex;
    align-items: center;
  }

  .mobile-drawer-header {
    justify-content: space-between;
    min-height: 52px;
  }

  .mobile-drawer-title { gap: 8px; color: var(--md-editor-fg); }
  .mobile-drawer-title span { color: var(--md-editor-muted-fg); font-size: 13px; }
  .mobile-drawer-header > button { display: grid; width: 44px; height: 44px; place-items: center; border: 0; background: transparent; color: var(--md-editor-fg); }

  .mobile-drawer-search {
    display: grid;
    grid-template-columns: 20px minmax(0, 1fr);
    gap: 9px;
    align-items: center;
    min-height: 48px;
    margin-bottom: 12px;
    padding: 0 12px;
    border: 1px solid var(--md-editor-border);
    border-radius: 14px;
    background: var(--md-editor-surface);
    color: var(--md-editor-muted-fg);
  }

  .mobile-drawer-search input { width: 100%; min-height: 44px; border: 0; outline: 0; background: transparent; color: var(--md-editor-fg); font: inherit; }
  .mobile-drawer-list { min-height: 0; overflow-y: auto; overscroll-behavior: contain; -webkit-overflow-scrolling: touch; }
  .mobile-drawer-group { margin-bottom: 16px; }
  .mobile-drawer-group h2 { display: flex; align-items: center; gap: 7px; overflow: hidden; margin: 0 6px 6px; color: var(--md-editor-muted-fg); font-size: 12px; font-weight: 600; }
  .mobile-drawer-group h2 span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .mobile-drawer-document { position: relative; display: grid; grid-template-columns: 20px minmax(0, 1fr) auto; gap: 10px; align-items: center; width: 100%; min-height: 48px; padding: 8px 10px; border: 0; border-radius: 12px; background: transparent; color: var(--md-editor-fg); text-align: left; font: inherit; cursor: pointer; }
  .mobile-drawer-document > span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .mobile-drawer-document.active { background: color-mix(in srgb, var(--md-editor-accent) 15%, transparent); color: var(--md-editor-accent-strong); }
  .mobile-drawer-dirty { width: 8px; height: 8px; border-radius: 50%; background: var(--md-editor-accent-strong); }
  .mobile-drawer-remove { display: grid; width: 30px; height: 30px; place-items: center; border: 0; background: transparent; color: var(--md-editor-muted-fg); }
  .mobile-drawer-empty { padding: 18px 10px; color: var(--md-editor-muted-fg); }

  @keyframes mobile-drawer-fade-in { to { opacity: 1; } }

  @media (prefers-reduced-motion: reduce) {
    .mobile-drawer-panel { transition: none; }
    .mobile-drawer-backdrop { animation: none; opacity: 1; }
  }
</style>
