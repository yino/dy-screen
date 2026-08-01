import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SearchableCombobox } from "./SearchableCombobox";

const options = [
  { value: "1", label: "主播甲", description: "带货 · 12 场" },
  { value: "2", label: "主播乙", description: "搞笑 · 8 场", disabled: true },
  { value: "3", label: "主播丙", description: "知识 · 3 场" },
];

function renderCombobox(overrides: Partial<React.ComponentProps<typeof SearchableCombobox>> = {}) {
  const props: React.ComponentProps<typeof SearchableCombobox> = {
    ariaLabel: "选择主播",
    placeholder: "先选择主播",
    searchPlaceholder: "搜索主播",
    value: null,
    options,
    emptyMessage: "没有主播",
    onChange: vi.fn(),
    onOpen: vi.fn(),
    onSearchChange: vi.fn(),
    onLoadMore: vi.fn(),
    onRetry: vi.fn(),
    ...overrides,
  };
  render(<SearchableCombobox {...props} />);
  return props;
}

describe("SearchableCombobox", () => {
  it("支持搜索、键盘跳过禁用项、回车选择和关闭后恢复焦点", async () => {
    const user = userEvent.setup();
    const props = renderCombobox();
    const trigger = screen.getByRole("combobox", { name: "选择主播" });
    await user.click(trigger);
    const search = screen.getByLabelText("选择主播搜索");
    expect(search).toHaveFocus();
    await user.type(search, "主播");
    await waitFor(() => expect(props.onSearchChange).toHaveBeenLastCalledWith("主播"));
    await user.keyboard("{ArrowDown}{Enter}");
    expect(props.onChange).toHaveBeenCalledWith("3");
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  it("支持 Esc、点击外部、失败重试、空结果和加载更多", async () => {
    const user = userEvent.setup();
    const retry = vi.fn();
    const loadMore = vi.fn();
    const { rerender } = render(<><SearchableCombobox
      ariaLabel="选择回放"
      placeholder="选择回放"
      searchPlaceholder="搜索日期"
      value={null}
      options={[]}
      error="目录读取失败"
      hasMore
      emptyMessage="没有回放"
      onChange={vi.fn()}
      onOpen={vi.fn()}
      onSearchChange={vi.fn()}
      onLoadMore={loadMore}
      onRetry={retry}
    /><button>外部按钮</button></>);
    const trigger = screen.getByRole("combobox", { name: "选择回放" });
    await user.click(trigger);
    await user.click(screen.getByRole("button", { name: "重试" }));
    expect(retry).toHaveBeenCalledTimes(1);
    await user.keyboard("{Escape}");
    expect(trigger).toHaveAttribute("aria-expanded", "false");

    rerender(<><SearchableCombobox
      ariaLabel="选择回放"
      placeholder="选择回放"
      searchPlaceholder="搜索日期"
      value={null}
      options={options}
      hasMore
      emptyMessage="没有回放"
      onChange={vi.fn()}
      onOpen={vi.fn()}
      onSearchChange={vi.fn()}
      onLoadMore={loadMore}
      onRetry={retry}
    /><button>外部按钮</button></>);
    await user.click(screen.getByRole("combobox", { name: "选择回放" }));
    await user.click(screen.getByRole("button", { name: "加载更多" }));
    expect(loadMore).toHaveBeenCalledTimes(1);
    fireEvent.mouseDown(screen.getByRole("button", { name: "外部按钮" }));
    expect(screen.getByRole("combobox", { name: "选择回放" })).toHaveAttribute("aria-expanded", "false");
  });
});
