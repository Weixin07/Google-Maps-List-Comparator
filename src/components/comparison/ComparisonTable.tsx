import { useMemo } from "react";
import type {
  ComparisonSegmentKey,
  PlaceComparisonRow,
} from "../../types/comparison";

export type TableFilters = {
  search: string;
  type: string;
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
  selectedIds: Set<string>;
  focusedPlaceId: string | null;
  onFiltersChange: (segment: ComparisonSegmentKey, filters: TableFilters) => void;
  onSelectionChange: (
    segment: ComparisonSegmentKey,
    placeIds: string[],
    checked: boolean,
  ) => void;
  onRowFocus: (segment: ComparisonSegmentKey, row: PlaceComparisonRow) => void;
};

export function ComparisonTable({
  segment,
  title,
  rows,
  totalCount,
  filters,
  availableTypes,
  selectedIds,
  focusedPlaceId,
  onFiltersChange,
  onSelectionChange,
  onRowFocus,
}: ComparisonTableProps) {
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

  const allVisibleSelected =
    sortedRows.length > 0 &&
    sortedRows.every((row) => selectedIds.has(row.place_id));

  const handleSearchChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    onFiltersChange(segment, { ...filters, search: event.target.value });
  };

  const handleTypeChange = (event: React.ChangeEvent<HTMLSelectElement>) => {
    onFiltersChange(segment, { ...filters, type: event.target.value });
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

  return (
    <div className="comparison-table">
      <div className="comparison-table__header">
        <div>
          <h3>{title}</h3>
          <p className="muted">
            Showing {sortedRows.length} of {totalCount} places
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
        <div className="comparison-table__scroll">
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
