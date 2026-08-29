import type {
  BlockAlignmentAnchor,
  BlockNaturalGeometry,
} from '../services/markdownBlockAlignment';

export interface MarkdownSourceSelection {
  from: number;
  to: number;
}

export interface MarkdownSourceEditorHandle {
  getMarkdown(): string;
  setMarkdown(markdown: string, options?: { addToHistory?: boolean }): void;
  getSelection(): MarkdownSourceSelection;
  setSelection(from: number, to?: number): void;
  getSelectedMarkdown(): string;
  focus(options?: FocusOptions): void;
  revealRange(from: number, to?: number): void;
  undo(): boolean;
  redo(): boolean;
  lineAtOffset(offset: number): number;
  offsetAtLine(lineNumber: number): number;
  getLineCount(): number;
  getLineTop(lineNumber: number): number;
  lineAtHeight(height: number): number;
  getLineHeight(): number;
  getScrollElement(): HTMLElement;
  getContentElement(): HTMLElement;
  getContentHeight(): number;
  getBlockGeometry(anchors: BlockAlignmentAnchor[]): BlockNaturalGeometry[];
  applyBlockGaps(
    anchors: BlockAlignmentAnchor[],
    gaps: ReadonlyMap<string, number>,
    leadingGap?: number,
  ): void;
  clearBlockGaps(): void;
  requestMeasure(): Promise<void> | void;
}
