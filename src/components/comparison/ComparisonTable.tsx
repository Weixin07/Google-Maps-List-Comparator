import { useEffect, useMemo, useState } from "react";
import type {
  ComparisonSegmentKey,
  PlaceComparisonRow,
} from "../../types/comparison";

export type TableFilters = {
  search: string;
  type: string;
  category: string;
  sortKey: "name" | "address";
  sortDirection: "asc" | "desc";
};

type ComparisonTableProps = {
  segment: ComparisonSegmentKey;
  title: string;
  rows: PlaceComparisonRow[];
  totalCount: number;
  filters: TableFilters;
  availableTypes: string[];
  availableCategories: string[];
  selectedIds: Set<string>;
  focusedPlaceId: string | null;
  page: number;
  pageSize: number;
  isLoading?: boolean;
  onFiltersChange: (segment: ComparisonSegmentKey, filters: TableFilters) => void;
  onSelectionChange: (
    segment: ComparisonSegmentKey,
    placeIds: string[],
    checked: boolean,
  ) => void;
  onRowFocus: (segment: ComparisonSegmentKey, row: PlaceComparisonRow) => void;
  onPageChange: (segment: ComparisonSegmentKey, page: number) => void;
};

export function ComparisonTable({
  segment,
  title,
  rows,
  totalCount,
  filters,
  availableTypes,
  availableCategories,
  selectedIds,
  focusedPlaceId,
  page,
  pageSize,
  isLoading,
  onFiltersChange,
  onSelectionChange,
  onRowFocus,
  onPageChange,
}: ComparisonTableProps) {
  const [activeIndex, setActiveIndex] = useState(0);

  const sortedRows = useMemo(() => {
    const copy = [...rows];
    const direction = filters.sortDirection === "asc" ? 1 : -1;
    const collator = new Intl.Collator(undefined, { sensitivity: "base" });
    return copy.sort((a, b) => {
      const aValue =
        filters.sortKey === "name"
          ? a.name
          : a.formatted_address ?? "No address available";
      const bValue =
        filters.sortKey === "name"
          ? b.name
          : b.formatted_address ?? "No address available";
      return collator.compare(aValue, bValue) * direction;
    });
  }, [rows, filters.sortDirection, filters.sortKey]);

  useEffect(() => {
    if (!focusedPlaceId) {
      return;
    }
    const index = sortedRows.findIndex((row) => row.place_id === focusedPlaceId);
    if (index >= 0) {
      setActiveIndex(index);
    }
  }, [focusedPlaceId, sortedRows]);

  const allVisibleSelected =
    sortedRows.length > 0 &&
    sortedRows.every((row) => selectedIds.has(row.place_id));

  const totalPages = Math.max(1, Math.ceil(totalCount / Math.max(1, pageSize)));
  const currentPage = Math.min(Math.max(1, page), totalPages);
  const start =
    totalCount === 0 || sortedRows.length === 0
      ? 0
      : (currentPage - 1) * pageSize + 1;
  const end = sortedRows.length === 0 ? 0 : Math.min(totalCount, start + sortedRows.length - 1);

  const handleSearchChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    onFiltersChange(segment, { ...filters, search: event.target.value });
  };

  const handleTypeChange = (event: React.ChangeEvent<HTMLSelectElement>) => {
    onFiltersChange(segment, { ...filters, type: event.target.value });
  };

  const handleCategoryChange = (event: React.ChangeEvent<HTMLSelectElement>) => {
    onFiltersChange(segment, { ...filters, category: event.target.value });
  };

  const toggleSort = (key: TableFilters["sortKey"]) => {
    if (filters.sortKey === key) {
      const nextDirection = filters.sortDirection === "asc" ? "desc" : "asc";
      onFiltersChange(segment, { ...filters, sortDirection: nextDirection });
    } else {
      onFiltersChange(segment, { ...filters, sortKey: key, sortDirection: "asc" });
    }
  };

  const handleRowSelect = (row: PlaceComparisonRow, checked: boolean) => {
    onSelectionChange(segment, [row.place_id], checked);
  };

  const handleSelectAll = (event: React.ChangeEvent<HTMLInputElement>) => {
    onSelectionChange(
      segment,
      sortedRows.map((row) => row.place_id),
      event.target.checked,
    );
  };

  const handleTableKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (sortedRows.length === 0) {
      return;
    }
    const target = event.target as HTMLElement;
    if (
      target.tagName === "INPUT" ||
      target.tagName === "SELECT" ||
      target.tagName === "BUTTON"
    ) {
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      const next = Math.min(sortedRows.length - 1, activeIndex + 1);
      setActiveIndex(next);
      onRowFocus(segment, sortedRows[next]);
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      const prev = Math.max(0, activeIndex - 1);
      setActiveIndex(prev);
      onRowFocus(segment, sortedRows[prev]);
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const row = sortedRows[activeIndex];
      if (row) {
        onRowFocus(segment, row);
      }
    }
  };

  return (
    <div className="comparison-table">
      <div className="comparison-table__header">
        <div>
          <h3>{title}</h3>
          <p className="muted">
            Showing {start === 0 ? 0 : `${start}-${end}`} of {totalCount} places ·
            Page {currentPage} of {totalPages}
          </p>
        </div>
        <div className="comparison-table__filters">
          <input
            type="search"
            className="table-search"
            placeholder="Search by name, address, or type"
            value={filters.search}
            onChange={handleSearchChange}
          />
          <select value={filters.type} onChange={handleTypeChange}>
            <option value="">All types</option>
            {availableTypes.map((type) => (
              <option key={`${segment}-${type}`} value={type}>
                {type}
              </option>
            ))}
          </select>
          <select value={filters.category} onChange={handleCategoryChange}>
            <option value="">All categories</option>
            {availableCategories.map((category) => (
              <option key={`${segment}-cat-${category}`} value={category}>
                {category}
              </option>
            ))}
          </select>
        </div>
      </div>
      <div className="comparison-table__actions">
        <label>
          <input
            type="checkbox"
            checked={allVisibleSelected}
            onChange={handleSelectAll}
          />{" "}
          Select visible
        </label>
        <div className="comparison-table__pagination">
          <button
            type="button"
            className="secondary-button"
            onClick={() => onPageChange(segment, Math.max(1, currentPage - 1))}
            disabled={currentPage <= 1 || Boolean(isLoading)}
          >
            Previous
          </button>
          <span className="muted">Page {currentPage} of {totalPages}</span>
          <button
            type="button"
            className="secondary-button"
            onClick={() =>
              onPageChange(segment, Math.min(totalPages, currentPage + 1))
            }
            disabled={currentPage >= totalPages || Boolean(isLoading)}
          >
            Next
          </button>
        </div>
        {selectedIds.size > 0 && (
          <button
            type="button"
            className="link-button"
            onClick={() => onSelectionChange(segment, [], false)}
          >
            Clear selection ({selectedIds.size})
          </button>
        )}
      </div>
      {sortedRows.length === 0 ? (
        <p className="muted">No places match the current filters.</p>
      ) : (
        <div
          className="comparison-table__scroll"
          tabIndex={0}
          onKeyDown={handleTableKeyDown}
        >
          <table>
            <thead>
              <tr>
                <th />
                <th>
                  <button
                    type="button"
                    className="table-sort"
                    onClick={() => toggleSort("name")}
                  >
                    Name
                    {filters.sortKey === "name" &&
                      (filters.sortDirection === "asc" ? " ↑" : " ↓")}
                  </button>
                </th>
                <th>
                  <button
                    type="button"
                    className="table-sort"
                    onClick={() => toggleSort("address")}
                  >
                    Address
                    {filters.sortKey === "address" &&
                      (filters.sortDirection === "asc" ? " ↑" : " ↓")}
                  </button>
                </th>
                <th>Types</th>
              </tr>
            </thead>
            <tbody>
              {sortedRows.map((row) => {
                const isSelected = selectedIds.has(row.place_id);
                const isFocused = focusedPlaceId === row.place_id;
                return (
                  <tr
                    key={row.place_id}
                    className={isFocused ? "focused-row" : undefined}
                  >
                    <td>
                      <input
                        type="checkbox"
                        checked={isSelected}
                        onChange={(event) =>
                          handleRowSelect(row, event.target.checked)
                        }
                      />
                    </td>
                    <td>
                      <button
                        type="button"
                        className="link-button"
                        onClick={() => onRowFocus(segment, row)}
                      >
                        {row.name}
                      </button>
                    </td>
                    <td>{row.formatted_address ?? "No address available"}</td>
                    <td>
                      {row.types.length === 0 ? (
                        <span className="muted">None</span>
                      ) : (
                        row.types.join(", ")
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
