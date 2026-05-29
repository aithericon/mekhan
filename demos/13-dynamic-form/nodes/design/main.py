"""Form designer — emits a dynamic HumanTask form at RUNTIME.

This is the load-bearing half of the dynamic-form feature: instead of the
HumanTask baking its `steps` as a compile-time literal, an upstream producer
emits the block list as data, and the downstream HumanTask sources it via
`stepsRef = "design.form"`.

The node declares a single output field `form: json` (see graph.json). The SDK
surfaces any top-level global matching a declared output name, so assigning
`form = [...]` here parks it as `design.form` for the read-arc the compiler
synthesized from the HumanTask's `stepsRef`.

`form` is a `TaskStepConfig[]` — exactly the schema the platform now advertises
to an Agent on the HumanTask input port (`task_step_list_json_schema`) and
enforces at runtime via the colored-token SchemaRegistry. A real agentic
workflow would have an Agent node emit this same array as a tool output; here a
deterministic script stands in so the demo runs fully offline.
"""

# In a real workflow these could be tailored per-instance (e.g. an LLM deciding
# which questions to ask based on upstream context). Here we emit a fixed but
# multi-block form to exercise text + select + checkbox inputs and a markdown
# display block — proving the renderer handles an arbitrary runtime block list.
form = [
    {
        "id": "step-triage",
        "title": "Triage",
        "descriptionMdsvex": "This form was **designed at runtime** by an upstream step.",
        "blocks": [
            {
                "type": "mdsvex",
                "content": "Please classify the incoming item and add any notes.",
            },
            {
                "type": "input",
                "field": {
                    "name": "category",
                    "label": "Category",
                    "kind": "select",
                    "required": True,
                    "options": [
                        {"value": "bug", "label": "Bug"},
                        {"value": "feature", "label": "Feature request"},
                        {"value": "question", "label": "Question"},
                    ],
                },
            },
            {
                "type": "input",
                "field": {
                    "name": "urgent",
                    "label": "Mark as urgent?",
                    "kind": "checkbox",
                    "required": False,
                },
            },
        ],
    },
    {
        "id": "step-notes",
        "title": "Notes",
        "blocks": [
            {
                "type": "input",
                "field": {
                    "name": "notes",
                    "label": "Reviewer notes",
                    "kind": "textarea",
                    "required": False,
                    "placeholder": "Anything the next step should know…",
                },
            }
        ],
    },
]
