import clsx from "clsx";
import styles from "./StartUpProgressContainer.module.scss";

import { useInitProgress } from "@logics_common";
import chat_white_square from "@images/vrct-0-icon.svg";
import { BrandLogo } from "@common_components";

export const StartUpProgressContainer = () => {
    const { currentInitProgress } = useInitProgress();

    const progress = currentInitProgress.data;
    return (
        <div className={styles.container}>
            <div className={styles.progress_bar_wrapper}>
                {[...Array(4)].map((_, index) => (
                    <div
                        key={index}
                        className={clsx(styles.progress_bar, {
                            [styles.progressed]: index < progress && progress !== 0,
                        })}
                    >
                        {index === 3
                            ?
                            <div className={styles.chato_box}>
                            <img src={chat_white_square} className={styles.chato_img}/>
                            </div>
                            : null
                        }
                    </div>
                ))}
            </div>
            <div className={styles.labels_wrapper}>
                <BrandLogo className={styles.vrct_starting_up_img} />
            </div>
        </div>
    );
};
