/**
 * One section for each generated screen of the platform fixture.
 *
 * Each screen runs over a stub transport from the shared module, the same
 * stubs that the component tests use. Each screen raises its own toast for
 * an outcome. The gallery reads the platform fixture only and never an
 * application.
 */

import type { JSX } from "solid-js";

import {
  WidgetArchiveForm,
  WidgetArchiveFormLabel,
  WidgetCreateForm,
  WidgetCreateFormLabel,
  WidgetDeleteDelete,
  WidgetDeleteDeleteLabel,
  WidgetGetDetail,
  WidgetGetDetailLabel,
  WidgetListTable,
  WidgetListTableLabel,
  WidgetQueryTable,
  WidgetQueryTableLabel,
  WidgetRecordBatchForm,
  WidgetRecordBatchFormLabel,
  WidgetUpdateForm,
  WidgetUpdateFormLabel,
} from "../fixture/components/widget.js";
import {
  WidgetMakerListTable,
  WidgetMakerListTableLabel,
  WidgetMakerQueryTable,
  WidgetMakerQueryTableLabel,
} from "../fixture/components/widget_maker.js";
import {
  deleteStub,
  groupStub,
  MAKER,
  page,
  paged,
  sampleStub,
  selectorStub,
  tableStub,
  WIDGET,
} from "../stubs/index.js";
import { Section } from "./section.js";

export function ScreenSections(): JSX.Element {
  return (
    <>
      <Section title={WidgetQueryTableLabel} name="WidgetQueryTable">
        <WidgetQueryTable
          transport={
            tableStub([page(["widget-001", "widget-002"], "c1"), page(["widget-003"], null)])
              .transport
          }
        />
      </Section>

      <Section title={WidgetListTableLabel} name="WidgetListTable">
        <WidgetListTable transport={groupStub().transport} />
      </Section>

      <Section title={WidgetMakerQueryTableLabel} name="WidgetMakerQueryTable">
        <WidgetMakerQueryTable transport={paged().transport} />
      </Section>

      <Section title={WidgetMakerListTableLabel} name="WidgetMakerListTable">
        <WidgetMakerListTable
          transport={sampleStub({
            status: "completed",
            value: { rows: [{ id: MAKER, name: "Northwind" }] },
          })}
        />
      </Section>

      <Section title={WidgetGetDetailLabel} name="WidgetGetDetail">
        <WidgetGetDetail transport={deleteStub().transport} input={{ id: WIDGET }} />
      </Section>

      <Section title={WidgetCreateFormLabel} name="WidgetCreateForm">
        <WidgetCreateForm transport={selectorStub().transport} />
      </Section>

      <Section title={WidgetUpdateFormLabel} name="WidgetUpdateForm">
        <WidgetUpdateForm transport={selectorStub().transport} key={{ id: WIDGET }} />
      </Section>

      <Section title={WidgetArchiveFormLabel} name="WidgetArchiveForm">
        <WidgetArchiveForm
          transport={selectorStub().transport}
          initial={{ id: WIDGET }}
          expectedEditVersion="7"
        />
      </Section>

      <Section title={WidgetRecordBatchFormLabel} name="WidgetRecordBatchForm">
        <WidgetRecordBatchForm transport={groupStub().transport} valueExpectedEditVersion="7" />
      </Section>

      <Section title={WidgetDeleteDeleteLabel} name="WidgetDeleteDelete">
        <WidgetDeleteDelete transport={deleteStub().transport} key={{ id: WIDGET }} />
      </Section>
    </>
  );
}
