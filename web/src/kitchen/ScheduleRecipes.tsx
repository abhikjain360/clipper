import { Link } from "wouter";
import type { KitchenPlan, OccurrenceView } from "@clipper/shared";

export function ScheduleRecipes({
    occurrence,
    plans,
}: {
    occurrence: OccurrenceView;
    plans: KitchenPlan[];
}) {
    const matches = plans.filter(
        (plan) =>
            plan.item_id === occurrence.item_id &&
            plan.occurrence_key === occurrence.occurrence_key,
    );
    if (matches.length === 0) return null;
    return (
        <div className="kitchen-schedule-recipes">
            {matches.map((plan, index) => (
                <Link
                    key={`${plan.recipe_id}:${index}`}
                    href={`/kitchen/${plan.recipe_id}`}
                    className="kitchen-schedule-link"
                    onClick={(event) => event.stopPropagation()}
                    onKeyDown={(event) => event.stopPropagation()}
                >
                    {plan.title}
                </Link>
            ))}
        </div>
    );
}
