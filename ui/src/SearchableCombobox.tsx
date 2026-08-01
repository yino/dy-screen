import { ChevronDown, LoaderCircle, RefreshCw, Search } from "lucide-react";
import {
  type KeyboardEvent,
  type ReactNode,
  type UIEvent,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";

export interface SearchableComboboxOption {
  value: string;
  label: string;
  description?: string;
  status?: ReactNode;
  disabled?: boolean;
}

interface SearchableComboboxProps {
  ariaLabel: string;
  placeholder: string;
  searchPlaceholder: string;
  value: string | null;
  selectedLabel?: string;
  options: SearchableComboboxOption[];
  disabled?: boolean;
  loading?: boolean;
  loadingMore?: boolean;
  error?: string | null;
  hasMore?: boolean;
  emptyMessage: string;
  onChange(value: string): void;
  onOpen(): void;
  onSearchChange(search: string): void;
  onLoadMore(): void;
  onRetry(): void;
}

export function SearchableCombobox({
  ariaLabel,
  placeholder,
  searchPlaceholder,
  value,
  selectedLabel,
  options,
  disabled = false,
  loading = false,
  loadingMore = false,
  error = null,
  hasMore = false,
  emptyMessage,
  onChange,
  onOpen,
  onSearchChange,
  onLoadMore,
  onRetry,
}: SearchableComboboxProps) {
  const id = useId().replace(/:/g, "");
  const listboxId = `${id}-listbox`;
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const onSearchChangeRef = useRef(onSearchChange);
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const [activeIndex, setActiveIndex] = useState(-1);

  const selected = options.find((option) => option.value === value);
  const displayLabel = selectedLabel ?? selected?.label ?? placeholder;
  const enabledIndexes = options.flatMap((option, index) => option.disabled ? [] : [index]);
  onSearchChangeRef.current = onSearchChange;

  useEffect(() => {
    if (!open) return;
    const timer = window.setTimeout(() => onSearchChangeRef.current(search), 180);
    return () => window.clearTimeout(timer);
  }, [open, search]);

  useEffect(() => {
    if (!open) return;
    inputRef.current?.focus();
  }, [open]);

  useEffect(() => {
    if (!open) return;
    setActiveIndex((current) => {
      if (current >= 0 && options[current] && !options[current].disabled) return current;
      return enabledIndexes[0] ?? -1;
    });
  }, [open, options]);

  useEffect(() => {
    if (!open) return;
    const closeOnOutside = (event: MouseEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) close(true);
    };
    document.addEventListener("mousedown", closeOnOutside);
    return () => document.removeEventListener("mousedown", closeOnOutside);
  }, [open]);

  const openList = () => {
    if (disabled) return;
    setSearch("");
    setOpen(true);
    onOpen();
  };

  const close = (restoreFocus: boolean) => {
    setOpen(false);
    setActiveIndex(-1);
    if (restoreFocus) window.setTimeout(() => triggerRef.current?.focus(), 0);
  };

  const select = (option: SearchableComboboxOption) => {
    if (option.disabled) return;
    onChange(option.value);
    close(true);
  };

  const move = (direction: -1 | 1) => {
    if (enabledIndexes.length === 0) return;
    const current = enabledIndexes.indexOf(activeIndex);
    const next = current < 0
      ? direction > 0 ? 0 : enabledIndexes.length - 1
      : (current + direction + enabledIndexes.length) % enabledIndexes.length;
    const nextIndex = enabledIndexes[next];
    setActiveIndex(nextIndex);
    document.getElementById(`${id}-option-${nextIndex}`)?.scrollIntoView?.({ block: "nearest" });
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      move(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      move(-1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      const option = options[activeIndex];
      if (option) select(option);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close(true);
    }
  };

  const handleScroll = (event: UIEvent<HTMLDivElement>) => {
    const target = event.currentTarget;
    if (hasMore && !loadingMore && target.scrollHeight - target.scrollTop - target.clientHeight < 32) {
      onLoadMore();
    }
  };

  return <div ref={rootRef} className={`searchable-combobox ${open ? "open" : ""}`}>
    <button
      ref={triggerRef}
      type="button"
      className="searchable-combobox-trigger"
      role="combobox"
      aria-label={ariaLabel}
      aria-expanded={open}
      aria-controls={listboxId}
      aria-haspopup="listbox"
      disabled={disabled}
      onClick={() => open ? close(false) : openList()}
      onKeyDown={(event) => {
        if (!open && ["ArrowDown", "Enter", " "].includes(event.key)) {
          event.preventDefault();
          openList();
        }
      }}
    >
      <span className={value ? "selected" : "placeholder"}>{displayLabel}</span>
      <ChevronDown size={15} aria-hidden="true" />
    </button>
    {open && <div className="searchable-combobox-popover" onKeyDown={(event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        close(true);
      }
    }}>
      <label className="searchable-combobox-search">
        <Search size={14} aria-hidden="true" />
        <input
          ref={inputRef}
          aria-label={`${ariaLabel}搜索`}
          aria-controls={listboxId}
          aria-activedescendant={activeIndex >= 0 ? `${id}-option-${activeIndex}` : undefined}
          placeholder={searchPlaceholder}
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          onKeyDown={handleKeyDown}
        />
      </label>
      <div id={listboxId} className="searchable-combobox-list" role="listbox" onScroll={handleScroll}>
        {loading && options.length === 0 ? <div className="searchable-combobox-state"><LoaderCircle className="spin" size={16} />正在加载</div>
          : error ? <div className="searchable-combobox-state error"><span>{error}</span><button type="button" onClick={onRetry}><RefreshCw size={13} />重试</button></div>
            : options.length === 0 ? <div className="searchable-combobox-state">{emptyMessage}</div>
              : options.map((option, index) => <button
                id={`${id}-option-${index}`}
                key={option.value}
                type="button"
                role="option"
                aria-selected={option.value === value}
                aria-disabled={option.disabled || undefined}
                className={`${index === activeIndex ? "active" : ""} ${option.disabled ? "disabled" : ""}`}
                disabled={option.disabled}
                onMouseEnter={() => !option.disabled && setActiveIndex(index)}
                onClick={() => select(option)}
              >
                <span><strong>{option.label}</strong>{option.description && <small>{option.description}</small>}</span>
                {option.status && <em>{option.status}</em>}
              </button>)}
        {hasMore && !error && <button type="button" className="searchable-combobox-more" disabled={loadingMore} onClick={onLoadMore}>
          {loadingMore ? <><LoaderCircle className="spin" size={13} />正在加载</> : "加载更多"}
        </button>}
      </div>
    </div>}
  </div>;
}
