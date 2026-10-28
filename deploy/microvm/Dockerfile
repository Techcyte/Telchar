FROM public.ecr.aws/lambda/microvms:al2023-minimal@sha256:c5b6c7ed901bd369018a22de329e8587dd7a848caaf21c7dab1153dc74385c36

RUN dnf install -y python3.12 shadow-utils && dnf clean all
RUN /usr/bin/python3.12 -c 'import os; assert hasattr(os, "unshare") and hasattr(os, "CLONE_NEWPID")'
RUN groupadd --gid 30000 nixbld && \
    for n in $(seq 1 16); do \
      useradd --uid "$((30000+n))" --gid nixbld --groups nixbld \
        --no-create-home --home-dir /var/empty --shell /sbin/nologin "nixbld$n"; \
    done
COPY nix/store/ /nix/store/
COPY runtime/ /opt/telchar/
RUN mkdir -p /etc/nix /var/empty && \
    cp /opt/telchar/nix.conf /etc/nix/nix.conf && \
    chmod 0700 /opt/telchar/entrypoint
ENV HOME=/root PYTHONDONTWRITEBYTECODE=1
USER 0:0
ENTRYPOINT ["/bin/sh", "/opt/telchar/entrypoint"]
