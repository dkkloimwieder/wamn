/**
 * The platform UI that generated components render through.
 *
 * The components are Zaidan source, copied into this package and owned here.
 * The generator names these exports, and this package owns how they look.
 * The stylesheet is `@wamn/ui/styles.css`.
 */

export { Badge } from "./components/ui/badge";
export { Button } from "./components/ui/button";
export { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./components/ui/card";
export { Field, FieldError, FieldGroup, FieldLabel, FieldLegend, FieldSet } from "./components/ui/field";
export { Input } from "./components/ui/input";
export { Toaster } from "./components/ui/toast";
export { ColorModeProvider, getClientColorMode, useColorMode } from "./components/color-mode";
export {
  FormActions,
  FormDone,
  TableScreen,
  type FormActionsProps,
  type FormDoneProps,
  type TableScreenProps,
} from "./actions";
export { ConfirmAction, type ConfirmActionProps } from "./confirm";
export { DetailItem, DetailList, type DetailItemProps, type DetailListProps } from "./detail";
export {
  CheckField,
  ChoiceField,
  TextField,
  type CheckFieldProps,
  type Choice,
  type ChoiceFieldProps,
  type TextFieldProps,
} from "./fields";
export {
  AppFrame,
  type AppFrameProps,
  type FrameEntry,
  type FrameItem,
  type FrameLink,
  type FrameSection,
} from "./frame";
export { CardPage, ScreenActions, type CardPageProps, type ScreenActionsProps } from "./page";
export { announceOutcome } from "./outcome";
export { createRecordLabels } from "./record-labels";
export { RecordSelect, type RecordSelectProps } from "./record-select";
export { ROW_HEIGHT, WINDOW_FROM } from "./table/grid";
export {
  builtColumns,
  type BuiltColumn,
  type TableColumn,
  type TableColumnRole,
  type TableColumnType,
  type TableSort,
  type TableSortDirection,
} from "./table/columns";
export { SetTable, type SetTableProps } from "./table/set-table";
export { defaultSetView, type GroupLevel, type SetViewState } from "./table/set-view";
export { defaultGridView, type GridViewState } from "./table/grid-view";
export { type QueryViewState } from "./table/query-view";
export { type SetFilter } from "./table/column-filter";
export { type Aggregate, type Bucket } from "./table/aggregate";
export { type GroupSort } from "./table/group-bar";
export { type ScopeFilter, type ScopeMatch, type ScopeMode, type ScopeRange } from "./table/scope-bar";
export { type BulkAction, type RowResult } from "./table/bulk";
export { type EditResult } from "./table/edit-cell";
export {
  QueryTable,
  type QueryTableAction,
  type QueryTableChild,
  type QueryTableChoices,
  type QueryTableColumn,
  type QueryTableDefinition,
  type QueryTableEditField,
  type QueryTableFill,
  type QueryTableFilter,
  type QueryTableMatch,
  type QueryTableProps,
  type QueryTableSearch,
  type QueryTableUpdate,
} from "./table/query-table";
