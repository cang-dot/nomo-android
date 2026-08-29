import { EditorView } from '@codemirror/view';
import { cleanup, render } from '@testing-library/svelte/pure';
import { afterEach, describe, expect, it, vi } from 'vitest';
import MarkdownSourceEditor from './MarkdownSourceEditor.svelte';
import type { MarkdownSourceEditorHandle } from './markdownSourceEditor';

afterEach(cleanup);

describe('MarkdownSourceEditor', () => {
  it('synchronizes CodeMirror input and preserves offsets when block spacers change', () => {
    let handle: MarkdownSourceEditorHandle | undefined;
    const onMarkdownChange = vi.fn();
    const { container } = render(MarkdownSourceEditor, {
      props: {
        markdown: '# Title\n\nBody',
        sourceEditor: undefined as unknown as MarkdownSourceEditorHandle,
        readonlyDocumentMode: false,
        onMarkdownChange,
        onReady: (value) => (handle = value),
      },
    });
    const view = EditorView.findFromDOM(container.querySelector('.cm-editor')!)!;

    view.dispatch({ changes: { from: view.state.doc.length, insert: '!' } });
    expect(onMarkdownChange).toHaveBeenLastCalledWith('# Title\n\nBody!');
    expect(handle?.getMarkdown()).toBe('# Title\n\nBody!');

    handle?.setSelection(10, 14);
    handle?.applyBlockGaps(
      [
        { key: 'node:0', nodeIndex: 0, fromLine: 1, toLine: 1 },
        { key: 'node:1', nodeIndex: 1, fromLine: 3, toLine: 3 },
      ],
      new Map([
        ['node:0', 28],
        ['node:1', 40],
      ]),
    );
    expect(handle?.getSelection()).toEqual({ from: 10, to: 14 });
    expect(handle?.getMarkdown()).toBe('# Title\n\nBody!');
    expect(container.querySelectorAll('.source-block-alignment-spacer')).toHaveLength(1);
    expect(
      (container.querySelector('.source-block-alignment-spacer') as HTMLElement).style.height,
    ).toBe('28px');
  });

  it('excludes scroll-past-end padding from natural block geometry', async () => {
    let handle: MarkdownSourceEditorHandle | undefined;
    const { container } = render(MarkdownSourceEditor, {
      props: {
        markdown: '# Title\n\nBody',
        sourceEditor: undefined as unknown as MarkdownSourceEditorHandle,
        readonlyDocumentMode: false,
        onReady: (value) => (handle = value),
      },
    });
    const view = EditorView.findFromDOM(container.querySelector('.cm-editor')!)!;
    view.contentDOM.style.paddingBottom = '80px';
    await new Promise<void>((resolve) => {
      view.requestMeasure({
        read: () => undefined,
        write: () => resolve(),
      });
    });

    const scale = view.scaleY;
    const paddingBottom = Number.parseFloat(getComputedStyle(view.contentDOM).paddingBottom);
    expect(view.contentHeight * scale - handle!.getContentHeight()).toBeCloseTo(
      paddingBottom * scale,
      1,
    );
    const geometry = handle!.getBlockGeometry([
      { key: 'node:0', nodeIndex: 0, fromLine: 1, toLine: 1 },
      { key: 'node:1', nodeIndex: 1, fromLine: 3, toLine: 3 },
    ]);
    expect(geometry.at(-1)?.nextTop).toBeCloseTo(handle!.getContentHeight(), 1);
  });

  it('keeps external synchronization out of history and supports source undo/redo', () => {
    let handle: MarkdownSourceEditorHandle | undefined;
    const { container } = render(MarkdownSourceEditor, {
      props: {
        markdown: 'alpha',
        sourceEditor: undefined as unknown as MarkdownSourceEditorHandle,
        readonlyDocumentMode: false,
        onReady: (value) => (handle = value),
      },
    });
    const view = EditorView.findFromDOM(container.querySelector('.cm-editor')!)!;

    handle?.setMarkdown('external', { addToHistory: false });
    view.dispatch({ changes: { from: 8, insert: ' edit' } });
    expect(handle?.getMarkdown()).toBe('external edit');
    expect(handle?.undo()).toBe(true);
    expect(handle?.getMarkdown()).toBe('external');
    expect(handle?.redo()).toBe(true);
    expect(handle?.getMarkdown()).toBe('external edit');
  });

  it('reads and restores selections and line offsets', () => {
    let handle: MarkdownSourceEditorHandle | undefined;
    render(MarkdownSourceEditor, {
      props: {
        markdown: 'one\ntwo\nthree',
        sourceEditor: undefined as unknown as MarkdownSourceEditorHandle,
        readonlyDocumentMode: false,
        onReady: (value) => (handle = value),
      },
    });

    handle?.setSelection(4, 7);
    expect(handle?.getSelectedMarkdown()).toBe('two');
    expect(handle?.lineAtOffset(5)).toBe(2);
    expect(handle?.offsetAtLine(3)).toBe(8);
  });

  it('does not duplicate Markdown changes across composition lifecycle events', () => {
    let handle: MarkdownSourceEditorHandle | undefined;
    const onMarkdownChange = vi.fn();
    const { container } = render(MarkdownSourceEditor, {
      props: {
        markdown: '',
        sourceEditor: undefined as unknown as MarkdownSourceEditorHandle,
        readonlyDocumentMode: false,
        onMarkdownChange,
        onReady: (value) => (handle = value),
      },
    });
    const view = EditorView.findFromDOM(container.querySelector('.cm-editor')!)!;

    view.contentDOM.dispatchEvent(new CompositionEvent('compositionstart', { data: '' }));
    view.dispatch({ changes: { from: 0, insert: '中' } });
    view.contentDOM.dispatchEvent(new CompositionEvent('compositionend', { data: '中' }));

    expect(handle?.getMarkdown()).toBe('中');
    expect(onMarkdownChange).toHaveBeenCalledTimes(1);
  });

  it('keeps mode-switch history but resets it when the active document changes', async () => {
    let handle: MarkdownSourceEditorHandle | undefined;
    const rendered = render(MarkdownSourceEditor, {
      props: {
        markdown: 'first',
        documentId: 'tab-1',
        sourceEditor: undefined as unknown as MarkdownSourceEditorHandle,
        onReady: (value) => (handle = value),
      },
    });
    const view = EditorView.findFromDOM(rendered.container.querySelector('.cm-editor')!)!;
    view.dispatch({ changes: { from: view.state.doc.length, insert: ' edit' } });
    expect(handle?.undo()).toBe(true);
    expect(handle?.redo()).toBe(true);

    await rendered.rerender({
      markdown: 'second',
      documentId: 'tab-2',
      sourceEditor: handle as MarkdownSourceEditorHandle,
      onReady: (value) => (handle = value),
    });

    expect(handle?.getMarkdown()).toBe('second');
    expect(handle?.undo()).toBe(false);
  });
});
